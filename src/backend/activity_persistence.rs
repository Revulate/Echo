//! Account-scoped, bounded Activity persistence.
//!
//! Activity rows contain private chat text, so this module never derives an
//! owner from settings and never writes diagnostic payloads.  The visibility
//! transaction captures immutable rows; a dedicated worker owns disk I/O.

use crate::backend::lock_utils::MutexExt;
use crate::backend::models::{
    ChatMessage, HighlightType, MessageFlags, ReplyInfo, SystemMessageType,
};
use crate::backend::slots::compute_structure_hash;
use crate::backend::slots::parser::{parse_emotes_tag, parse_to_slots_with_gifs};
use crate::backend::whispers::ActivityWhisperThread;
use rustc_hash::FxHashMap;
use serde::{Deserialize, Serialize};
use smallvec::SmallVec;
use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, LazyLock, Mutex, OnceLock};
use std::time::{Duration, Instant};

const VERSION: u32 = 1;
const MAX_FILE_BYTES: usize = 16 * 1024 * 1024;
const MAX_MENTIONS: usize = 500;
const MAX_THREADS: usize = 50;
const MAX_THREAD_MESSAGES: usize = 100;
const MAX_PENDING_OWNERS: usize = 8;
const MAX_CONTENT_BYTES: usize = 2_048;
const MAX_FIELD_BYTES: usize = 512;
const MAX_BADGES: usize = 32;
const DEBOUNCE: Duration = Duration::from_secs(3);
const MAX_DEBOUNCE: Duration = Duration::from_secs(10);

#[derive(Clone)]
struct LoadedAccount {
    owner_user_id: String,
    auth_epoch: u64,
}

#[derive(Default)]
struct State {
    loaded: Option<LoadedAccount>,
}

#[derive(Clone)]
struct RawSnapshot {
    owner_user_id: String,
    mentions: Vec<(String, ChatMessage, bool)>,
    whisper_threads: Vec<ActivityWhisperThread>,
    last_received_sender: Option<String>,
}

struct PendingSnapshot {
    snapshot: Arc<RawSnapshot>,
    queued_at: Instant,
}

enum WorkerCommand {
    Dirty,
    Snapshot(Arc<RawSnapshot>),
    Flush(Sender<bool>),
}

static STATE: LazyLock<Mutex<State>> = LazyLock::new(|| Mutex::new(State::default()));
static HYDRATE: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));
static PENDING: LazyLock<Mutex<HashMap<String, PendingSnapshot>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
static UNWRITABLE_OWNERS: LazyLock<Mutex<HashMap<String, ()>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
static WORKER: OnceLock<Option<Sender<WorkerCommand>>> = OnceLock::new();
static DIRTY: AtomicBool = AtomicBool::new(false);
static DIRTY_GENERATION: AtomicU64 = AtomicU64::new(0);
static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Starts the dedicated persistence worker. Repeated calls are harmless.
pub(crate) fn start() {
    let _ = WORKER.get_or_init(|| {
        let (sender, receiver) = mpsc::channel();
        match std::thread::Builder::new()
            .name("echo-activity-persist".to_string())
            .spawn(move || worker(receiver))
        {
            Ok(_) => Some(sender),
            Err(error) => {
                warn(
                    "Activity persistence is unavailable",
                    Some(error.to_string()),
                );
                None
            }
        }
    });
}

fn worker_sender() -> Option<Sender<WorkerCommand>> {
    start();
    WORKER.get().and_then(|sender| sender.as_ref()).cloned()
}

/// Mark the current account snapshot dirty. This deliberately takes no store,
/// identity, or visibility locks because callers can already hold them.
pub(crate) fn mark_dirty() {
    DIRTY_GENERATION.fetch_add(1, Ordering::AcqRel);
    if DIRTY.swap(true, Ordering::AcqRel) {
        return;
    }
    let Some(sender) = worker_sender() else {
        return;
    };
    if sender.send(WorkerCommand::Dirty).is_err() {
        warn("Activity persistence stopped", None);
    }
}

/// Called while the visibility transaction is held before private Activity
/// stores are cleared for an account switch or logout.
pub(crate) fn retire_account_visibility_commit_held() {
    let owner = STATE
        .lock_or_recover()
        .loaded
        .take()
        .map(|loaded| loaded.owner_user_id);

    let snapshot = owner.and_then(|owner_user_id| {
        valid_owner(&owner_user_id).then(|| {
            let (whisper_threads, last_received_sender) =
                crate::backend::whispers::snapshot_activity_visibility_commit_held();
            RawSnapshot {
                owner_user_id,
                mentions: crate::backend::irc::activity_mentions_snapshot_visibility_commit_held(),
                whisper_threads,
                last_received_sender,
            }
        })
    });
    crate::backend::irc::clear_activity_mentions_visibility_commit_held();
    if let Some(snapshot) = snapshot {
        queue_snapshot(Arc::new(snapshot));
    }
}

/// Hydrates a newly authenticated account after blocked-user visibility has
/// committed. Same-owner token rotations retain resident Activity rows.
pub(crate) fn hydrate_current_account() {
    let _serial = HYDRATE.lock_or_recover();

    let Some((owner_user_id, auth_epoch)) = capture_current_session() else {
        return;
    };
    {
        let visibility_commit = crate::backend::blocked_users::visibility_commit_guard();
        let current = capture_current_session_visibility_commit_held();
        let state = STATE.lock_or_recover();
        if current
            .as_ref()
            .is_some_and(|(current_owner, current_epoch)| {
                current_owner == &owner_user_id && *current_epoch == auth_epoch
            })
            && state
                .loaded
                .as_ref()
                .is_some_and(|loaded| loaded.owner_user_id == owner_user_id)
        {
            drop(state);
            STATE.lock_or_recover().loaded = Some(LoadedAccount {
                owner_user_id,
                auth_epoch,
            });
            drop(visibility_commit);
            return;
        }
        drop(state);
        drop(visibility_commit);
    }

    let snapshot =
        pending_snapshot(&owner_user_id).or_else(|| match read_snapshot(&owner_user_id) {
            Ok(snapshot) => snapshot,
            Err(error) => {
                warn("Could not restore Activity", Some(error));
                None
            }
        });

    let restored = snapshot
        .as_deref()
        .map(snapshot_from_disk)
        .transpose()
        .unwrap_or_else(|error| {
            warn("Could not restore Activity", Some(error));
            None
        });

    let visibility_commit = crate::backend::blocked_users::visibility_commit_guard();
    let Some((current_owner, current_epoch)) = capture_current_session_visibility_commit_held()
    else {
        return;
    };
    if current_owner != owner_user_id || current_epoch != auth_epoch {
        return;
    }

    let sanitized = restored.as_ref().is_some_and(|restored| restored.sanitized);
    if let Some(restored) = restored {
        crate::backend::irc::restore_activity_mentions_visibility_commit_held(restored.mentions);
        crate::backend::whispers::restore_activity_visibility_commit_held(
            restored.whisper_threads,
            restored.last_received_sender,
        );
    }
    STATE.lock_or_recover().loaded = Some(LoadedAccount {
        owner_user_id,
        auth_epoch,
    });
    drop(visibility_commit);

    if snapshot.is_some() {
        crate::backend::irc::bump_chat_version();
        crate::backend::observers::get_observer_registry().notify(
            crate::backend::whispers::WHISPERS_CHANNEL,
            crate::backend::observers::ChannelEvent::MessagesRetokenized,
        );
        crate::state::ui_invalidation::request(
            crate::state::ui_invalidation::UiInvalidation::TopBarState,
        );
        crate::managers::frame_rate_controller::wakeup();
    }
    if sanitized {
        mark_dirty();
    }
}

/// Queues a final durable snapshot and waits for the worker's write barrier.
pub(crate) fn flush(timeout: Duration) -> bool {
    if let Some(snapshot) = capture_live_snapshot() {
        queue_snapshot(snapshot);
    }
    let Some(sender) = worker_sender() else {
        return false;
    };
    let (ack_sender, ack_receiver) = mpsc::channel();
    if sender.send(WorkerCommand::Flush(ack_sender)).is_err() {
        warn("Activity persistence stopped", None);
        return false;
    }
    ack_receiver.recv_timeout(timeout).unwrap_or(false)
}

fn capture_current_session() -> Option<(String, u64)> {
    let visibility_commit = crate::backend::blocked_users::visibility_commit_guard();
    let result = capture_current_session_visibility_commit_held();
    drop(visibility_commit);
    result
}

fn capture_current_session_visibility_commit_held() -> Option<(String, u64)> {
    let session = crate::backend::blocked_users::capture_active_session().ok()?;
    let owner_user_id = session.owner_user_id().trim().to_string();
    valid_owner(&owner_user_id).then_some((
        owner_user_id,
        crate::backend::blocked_users::current_auth_epoch_visibility_commit_held(),
    ))
}

fn capture_live_snapshot() -> Option<Arc<RawSnapshot>> {
    let visibility_commit = crate::backend::blocked_users::visibility_commit_guard();
    let current = capture_current_session_visibility_commit_held();
    let snapshot = current.and_then(|(owner_user_id, auth_epoch)| {
        let state = STATE.lock_or_recover();
        let current_owner = state.loaded.as_ref().filter(|loaded| {
            loaded.owner_user_id == owner_user_id && loaded.auth_epoch == auth_epoch
        })?;
        let _ = current_owner;
        let mentions = crate::backend::irc::activity_mentions_snapshot_visibility_commit_held();
        let (whisper_threads, last_received_sender) =
            crate::backend::whispers::snapshot_activity_visibility_commit_held();
        Some(Arc::new(RawSnapshot {
            owner_user_id,
            mentions,
            whisper_threads,
            last_received_sender,
        }))
    });
    drop(visibility_commit);
    snapshot
}

fn queue_snapshot(snapshot: Arc<RawSnapshot>) {
    let owner_user_id = snapshot.owner_user_id.clone();
    {
        let mut pending = PENDING.lock_or_recover();
        if !pending.contains_key(&owner_user_id)
            && pending.len() >= MAX_PENDING_OWNERS
            && let Some(oldest) = pending
                .iter()
                .min_by_key(|(_, snapshot)| snapshot.queued_at)
                .map(|(owner, _)| owner.clone())
        {
            pending.remove(&oldest);
            warn("Activity persistence queue is full", None);
        }
        pending.insert(
            owner_user_id,
            PendingSnapshot {
                snapshot: Arc::clone(&snapshot),
                queued_at: Instant::now(),
            },
        );
    }
    let Some(sender) = worker_sender() else {
        return;
    };
    if sender.send(WorkerCommand::Snapshot(snapshot)).is_err() {
        warn("Activity persistence stopped", None);
    }
}

fn pending_snapshot(owner_user_id: &str) -> Option<Arc<RawSnapshot>> {
    PENDING
        .lock_or_recover()
        .get(owner_user_id)
        .map(|pending| Arc::clone(&pending.snapshot))
}

fn worker(receiver: Receiver<WorkerCommand>) {
    let mut first_dirty = None;
    let mut last_dirty = None;
    let mut observed_dirty_generation = DIRTY_GENERATION.load(Ordering::Acquire);
    loop {
        let timeout = dirty_timeout(first_dirty, last_dirty);
        match timeout {
            Some(timeout) => match receiver.recv_timeout(timeout) {
                Ok(command) => {
                    if !handle_command(
                        command,
                        &mut first_dirty,
                        &mut last_dirty,
                        &mut observed_dirty_generation,
                    ) {
                        return;
                    }
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    let current_generation = DIRTY_GENERATION.load(Ordering::Acquire);
                    if current_generation != observed_dirty_generation
                        && first_dirty.is_some_and(|first| first.elapsed() < MAX_DEBOUNCE)
                    {
                        observed_dirty_generation = current_generation;
                        last_dirty = Some(Instant::now());
                        continue;
                    }
                    first_dirty = None;
                    last_dirty = None;
                    DIRTY.store(false, Ordering::Release);
                    if let Some(snapshot) = capture_live_snapshot() {
                        write_pending_snapshot(snapshot);
                    }
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => return,
            },
            None => match receiver.recv() {
                Ok(command) => {
                    if !handle_command(
                        command,
                        &mut first_dirty,
                        &mut last_dirty,
                        &mut observed_dirty_generation,
                    ) {
                        return;
                    }
                }
                Err(_) => return,
            },
        }
    }
}

fn dirty_timeout(first_dirty: Option<Instant>, last_dirty: Option<Instant>) -> Option<Duration> {
    let (Some(first_dirty), Some(last_dirty)) = (first_dirty, last_dirty) else {
        return None;
    };
    let now = Instant::now();
    let deadline = (first_dirty + MAX_DEBOUNCE).min(last_dirty + DEBOUNCE);
    Some(deadline.saturating_duration_since(now))
}

fn handle_command(
    command: WorkerCommand,
    first_dirty: &mut Option<Instant>,
    last_dirty: &mut Option<Instant>,
    observed_dirty_generation: &mut u64,
) -> bool {
    match command {
        WorkerCommand::Dirty => {
            let now = Instant::now();
            first_dirty.get_or_insert(now);
            *last_dirty = Some(now);
            *observed_dirty_generation = DIRTY_GENERATION.load(Ordering::Acquire);
        }
        WorkerCommand::Snapshot(snapshot) => {
            write_pending_snapshot(snapshot);
        }
        WorkerCommand::Flush(ack) => {
            if first_dirty.is_some() {
                *first_dirty = None;
                *last_dirty = None;
                DIRTY.store(false, Ordering::Release);
                if let Some(snapshot) = capture_live_snapshot() {
                    write_pending_snapshot(snapshot);
                }
            }
            let pending: Vec<_> = PENDING
                .lock_or_recover()
                .values()
                .map(|pending| Arc::clone(&pending.snapshot))
                .collect();
            let mut success = true;
            for snapshot in pending {
                success &= write_pending_snapshot(snapshot);
            }
            let _ = ack.send(success);
        }
    }
    true
}

fn write_pending_snapshot(requested: Arc<RawSnapshot>) -> bool {
    let owner_user_id = requested.owner_user_id.clone();
    if UNWRITABLE_OWNERS
        .lock_or_recover()
        .contains_key(&owner_user_id)
    {
        return false;
    }
    let snapshot = pending_snapshot(&owner_user_id).unwrap_or(requested);
    let result = write_snapshot(&snapshot);
    match result {
        Ok(()) => {
            let mut pending = PENDING.lock_or_recover();
            if pending
                .get(&owner_user_id)
                .is_some_and(|current| Arc::ptr_eq(&current.snapshot, &snapshot))
            {
                pending.remove(&owner_user_id);
            }
            true
        }
        Err(error) => {
            warn("Could not save Activity", Some(error));
            false
        }
    }
}

fn activity_path(owner_user_id: &str) -> Option<PathBuf> {
    if !valid_owner(owner_user_id) {
        return None;
    }
    Some(
        crate::benchmark_profile::config_root()?
            .join("activity")
            .join(format!("account-{owner_user_id}.json")),
    )
}

fn read_snapshot(owner_user_id: &str) -> Result<Option<Arc<RawSnapshot>>, String> {
    let Some(path) = activity_path(owner_user_id) else {
        return Ok(None);
    };
    let file = match File::open(&path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("{}", error.kind())),
    };
    let mut bytes = Vec::with_capacity(64 * 1024);
    file.take((MAX_FILE_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("{}", error.kind()))?;
    if bytes.len() > MAX_FILE_BYTES {
        return failed_load(&path, owner_user_id, "Activity snapshot is too large");
    }
    let stored: StoredSnapshot = serde_json::from_slice(&bytes).map_err(|error| {
        let detail = crate::utils::json_error_summary(&error);
        preserve_failed_snapshot(&path, owner_user_id);
        detail
    })?;
    if stored.version != VERSION
        || stored.owner_user_id != owner_user_id
        || !valid_owner(&stored.owner_user_id)
    {
        return failed_load(
            &path,
            owner_user_id,
            "Activity snapshot does not belong to this account",
        );
    }
    Ok(Some(Arc::new(stored.into_raw())))
}

fn failed_load<T>(path: &Path, owner_user_id: &str, detail: &str) -> Result<T, String> {
    preserve_failed_snapshot(path, owner_user_id);
    Err(detail.to_string())
}

fn preserve_failed_snapshot(path: &Path, owner_user_id: &str) {
    if !crate::utils::preserve_unloadable_file(path, "Activity snapshot") {
        UNWRITABLE_OWNERS
            .lock_or_recover()
            .insert(owner_user_id.to_string(), ());
    }
}

fn write_snapshot(snapshot: &RawSnapshot) -> Result<(), String> {
    let Some(path) = activity_path(&snapshot.owner_user_id) else {
        return Err("No private Activity storage root is available".to_string());
    };
    let mut stored = StoredSnapshot::from_raw(snapshot);
    let mut bytes =
        serde_json::to_vec(&stored).map_err(|error| crate::utils::json_error_summary(&error))?;
    while bytes.len() > MAX_FILE_BYTES {
        let rows = stored.mentions.len()
            + stored
                .whisper_threads
                .iter()
                .map(|thread| thread.messages.len())
                .sum::<usize>();
        if rows == 0 {
            break;
        }
        for _ in 0..rows.div_ceil(2) {
            if !discard_oldest_stored_row(&mut stored) {
                break;
            }
        }
        bytes = serde_json::to_vec(&stored)
            .map_err(|error| crate::utils::json_error_summary(&error))?;
    }
    if bytes.len() > MAX_FILE_BYTES {
        return Err("Activity snapshot exceeds its bounded storage limit".to_string());
    }
    let parent = path
        .parent()
        .ok_or_else(|| "Activity storage path has no parent".to_string())?;
    fs::create_dir_all(parent).map_err(|error| format!("{}", error.kind()))?;
    let temporary = parent.join(format!(
        ".activity-{}-{}.tmp",
        std::process::id(),
        TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    let result = (|| -> std::io::Result<()> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        drop(file);
        crate::utils::replace_file(&temporary, &path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result.map_err(|error| format!("{}", error.kind()))
}

fn snapshot_from_disk(snapshot: &RawSnapshot) -> Result<RestoredSnapshot, String> {
    let mut mentions = Vec::with_capacity(snapshot.mentions.len());
    let mut sanitized = false;
    for (channel, message, unread) in &snapshot.mentions {
        let message = retokenize(message.clone())?;
        if !crate::backend::blocked_users::is_blocked(message.user_id.as_deref(), &message.sender) {
            mentions.push((channel.clone(), message, *unread));
        } else {
            sanitized = true;
        }
    }
    let mut whisper_threads = Vec::with_capacity(snapshot.whisper_threads.len());
    for thread in &snapshot.whisper_threads {
        if crate::backend::blocked_users::is_blocked(thread.user_id.as_deref(), &thread.username) {
            sanitized = true;
            continue;
        }
        let mut thread = thread.clone();
        let mut messages = Vec::with_capacity(thread.messages.len());
        let stored_messages = std::mem::take(&mut thread.messages);
        for message in stored_messages {
            if crate::backend::blocked_users::is_blocked(
                message.user_id.as_deref(),
                &message.sender,
            ) {
                sanitized = true;
                continue;
            }
            messages.push(retokenize(message)?);
        }
        thread.messages = messages;
        whisper_threads.push(thread);
    }
    Ok(RestoredSnapshot {
        mentions,
        whisper_threads,
        last_received_sender: snapshot.last_received_sender.clone(),
        sanitized,
    })
}

struct RestoredSnapshot {
    mentions: Vec<(String, ChatMessage, bool)>,
    whisper_threads: Vec<ActivityWhisperThread>,
    last_received_sender: Option<String>,
    sanitized: bool,
}

fn retokenize(message: ChatMessage) -> Result<ChatMessage, String> {
    let twitch_emotes: FxHashMap<usize, (String, usize)> = message
        .twitch_emotes_tag
        .as_deref()
        .map(parse_emotes_tag)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|(start, (id, end))| {
            if end <= message.reply_emote_offset {
                return None;
            }
            let start = start.saturating_sub(message.reply_emote_offset);
            let end = end.saturating_sub(message.reply_emote_offset);
            (end > start).then_some((start, (id, end)))
        })
        .collect();
    let lookup_channel = message
        .emote_lookup_channel
        .as_deref()
        .unwrap_or(message.channel.as_ref());
    let slots = parse_to_slots_with_gifs(
        &message.content,
        lookup_channel,
        crate::backend::emotes::EmoteSender::new(message.user_id.as_deref(), &message.sender),
        &twitch_emotes,
        message.twitch_gifs_tag.as_deref(),
        message.reply_emote_offset,
    );
    let structure_hash = compute_structure_hash(&slots);
    Ok(ChatMessage {
        slots: slots.into(),
        structure_hash,
        ..message
    })
}

fn valid_owner(owner_user_id: &str) -> bool {
    !owner_user_id.is_empty() && owner_user_id.bytes().all(|byte| byte.is_ascii_digit())
}

fn warn(message: &str, detail: Option<String>) {
    crate::backend::push_ui_action(crate::backend::UiAction::ShowWarningToast {
        message: message.to_string(),
        detail,
    });
}

fn cap_text(value: &str, maximum: usize) -> String {
    if value.len() <= maximum {
        return value.to_string();
    }
    let mut end = maximum;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_string()
}

fn cap_optional(value: Option<&str>, maximum: usize) -> Option<String> {
    value.map(|value| cap_text(value, maximum))
}

/// Remove one oldest durable row when a pathological but individually bounded
/// snapshot would exceed the file cap. Thread summaries survive so Activity
/// still identifies the conversation after truncation.
fn discard_oldest_stored_row(snapshot: &mut StoredSnapshot) -> bool {
    let oldest_whisper = snapshot
        .whisper_threads
        .iter()
        .enumerate()
        .filter_map(|(thread_index, thread)| {
            thread
                .messages
                .first()
                .map(|message| (thread_index, message.timestamp))
        })
        .min_by_key(|(_, timestamp)| *timestamp);
    let oldest_mention = snapshot
        .mentions
        .iter()
        .enumerate()
        .min_by_key(|(_, mention)| mention.message.timestamp)
        .map(|(index, mention)| (index, mention.message.timestamp));
    match (oldest_whisper, oldest_mention) {
        (Some((thread_index, whisper_timestamp)), Some((_, mention_timestamp)))
            if whisper_timestamp <= mention_timestamp =>
        {
            snapshot.whisper_threads[thread_index].messages.remove(0);
            true
        }
        (Some((thread_index, _)), None) => {
            snapshot.whisper_threads[thread_index].messages.remove(0);
            true
        }
        (_, Some((mention_index, _))) => {
            snapshot.mentions.remove(mention_index);
            true
        }
        (None, None) => false,
    }
}

#[derive(Clone, Serialize, Deserialize)]
struct StoredSnapshot {
    version: u32,
    owner_user_id: String,
    mentions: Vec<StoredMention>,
    whisper_threads: Vec<StoredWhisperThread>,
    last_received_sender: Option<String>,
}

#[derive(Clone, Serialize, Deserialize)]
struct StoredMention {
    channel: String,
    message: StoredMessage,
    unread: bool,
}

#[derive(Clone, Serialize, Deserialize)]
struct StoredWhisperThread {
    username: String,
    user_id: Option<String>,
    last_message: String,
    unread_count: u32,
    last_timestamp: i64,
    messages: Vec<StoredMessage>,
}

#[derive(Clone, Serialize, Deserialize)]
struct StoredMessage {
    id: String,
    channel: String,
    emote_lookup_channel: Option<String>,
    emote_lookup_source_id: Option<String>,
    sender: String,
    display_name: String,
    content: String,
    color: Option<String>,
    badges: Vec<String>,
    timestamp: i64,
    reply: Option<StoredReply>,
    flags: StoredFlags,
    twitch_gifs_tag: Option<String>,
    twitch_emotes_tag: Option<String>,
    user_id: Option<String>,
    reply_emote_offset: usize,
}

#[derive(Clone, Serialize, Deserialize)]
struct StoredReply {
    parent_msg_id: String,
    parent_user_login: String,
    parent_display_name: String,
    parent_user_id: Option<String>,
    parent_msg_body: Option<String>,
}

#[derive(Clone, Serialize, Deserialize)]
struct StoredFlags {
    is_action: bool,
    highlight_type: Option<HighlightType>,
    is_system: bool,
    is_usernotice: bool,
    is_first_message: bool,
    system_type: Option<SystemMessageType>,
    is_shared_chat: bool,
    is_deleted: bool,
    is_elevated_message: bool,
    is_hype_chat: bool,
    is_automod: bool,
    is_automod_pending: bool,
    is_suspicious_user: bool,
    is_restricted: bool,
    is_monitored: bool,
    is_similar: bool,
    is_alternate_row: bool,
}

impl StoredSnapshot {
    fn from_raw(snapshot: &RawSnapshot) -> Self {
        let mut mentions = snapshot
            .mentions
            .iter()
            .map(|(channel, message, unread)| StoredMention {
                channel: cap_text(channel, MAX_FIELD_BYTES),
                message: StoredMessage::from_message(message),
                unread: *unread,
            })
            .collect::<Vec<_>>();
        mentions.sort_by_key(|mention| mention.message.timestamp);
        if mentions.len() > MAX_MENTIONS {
            mentions.drain(..mentions.len() - MAX_MENTIONS);
        }
        let mut whisper_threads = snapshot
            .whisper_threads
            .iter()
            .map(StoredWhisperThread::from_thread)
            .collect::<Vec<_>>();
        whisper_threads.sort_by_key(|thread| std::cmp::Reverse(thread.last_timestamp));
        whisper_threads.truncate(MAX_THREADS);
        Self {
            version: VERSION,
            owner_user_id: snapshot.owner_user_id.clone(),
            mentions,
            whisper_threads,
            last_received_sender: cap_optional(
                snapshot.last_received_sender.as_deref(),
                MAX_FIELD_BYTES,
            ),
        }
    }

    fn into_raw(self) -> RawSnapshot {
        RawSnapshot {
            owner_user_id: self.owner_user_id,
            mentions: self
                .mentions
                .into_iter()
                .take(MAX_MENTIONS)
                .filter_map(|mention| {
                    StoredMessage::into_message(mention.message).map(|message| {
                        (
                            cap_text(&mention.channel, MAX_FIELD_BYTES),
                            message,
                            mention.unread,
                        )
                    })
                })
                .collect(),
            whisper_threads: self
                .whisper_threads
                .into_iter()
                .take(MAX_THREADS)
                .filter_map(StoredWhisperThread::into_thread)
                .collect(),
            last_received_sender: self
                .last_received_sender
                .map(|value| cap_text(&value, MAX_FIELD_BYTES)),
        }
    }
}

impl StoredWhisperThread {
    fn from_thread(thread: &ActivityWhisperThread) -> Self {
        let mut messages = thread
            .messages
            .iter()
            .map(StoredMessage::from_message)
            .collect::<Vec<_>>();
        messages.sort_by(|left, right| {
            left.timestamp
                .cmp(&right.timestamp)
                .then_with(|| left.id.cmp(&right.id))
        });
        if messages.len() > MAX_THREAD_MESSAGES {
            messages.drain(..messages.len() - MAX_THREAD_MESSAGES);
        }
        Self {
            username: cap_text(&thread.username, MAX_FIELD_BYTES),
            user_id: cap_optional(thread.user_id.as_deref(), MAX_FIELD_BYTES),
            last_message: cap_text(&thread.last_message, MAX_CONTENT_BYTES),
            unread_count: thread.unread_count,
            last_timestamp: thread.last_timestamp,
            messages,
        }
    }

    fn into_thread(self) -> Option<ActivityWhisperThread> {
        let username = cap_text(&self.username, MAX_FIELD_BYTES);
        (!username.trim().is_empty()).then(|| ActivityWhisperThread {
            username,
            user_id: self.user_id.map(|value| cap_text(&value, MAX_FIELD_BYTES)),
            last_message: cap_text(&self.last_message, MAX_CONTENT_BYTES),
            unread_count: self.unread_count,
            last_timestamp: self.last_timestamp,
            messages: self
                .messages
                .into_iter()
                .take(MAX_THREAD_MESSAGES)
                .filter_map(StoredMessage::into_message)
                .collect(),
        })
    }
}

impl StoredMessage {
    fn from_message(message: &ChatMessage) -> Self {
        Self {
            id: cap_text(&message.id, MAX_FIELD_BYTES),
            channel: cap_text(&message.channel, MAX_FIELD_BYTES),
            emote_lookup_channel: cap_optional(
                message.emote_lookup_channel.as_deref(),
                MAX_FIELD_BYTES,
            ),
            emote_lookup_source_id: cap_optional(
                message.emote_lookup_source_id.as_deref(),
                MAX_FIELD_BYTES,
            ),
            sender: cap_text(&message.sender, MAX_FIELD_BYTES),
            display_name: cap_text(&message.display_name, MAX_FIELD_BYTES),
            content: cap_text(&message.content, MAX_CONTENT_BYTES),
            color: cap_optional(message.color.as_deref(), MAX_FIELD_BYTES),
            badges: message
                .badges
                .iter()
                .take(MAX_BADGES)
                .map(|badge| cap_text(badge, MAX_FIELD_BYTES))
                .collect(),
            timestamp: message.timestamp,
            reply: message.reply.as_ref().map(StoredReply::from_reply),
            flags: StoredFlags::from_flags(&message.flags),
            twitch_gifs_tag: cap_optional(message.twitch_gifs_tag.as_deref(), MAX_CONTENT_BYTES),
            twitch_emotes_tag: cap_optional(
                message.twitch_emotes_tag.as_deref(),
                MAX_CONTENT_BYTES,
            ),
            user_id: cap_optional(message.user_id.as_deref(), MAX_FIELD_BYTES),
            reply_emote_offset: message.reply_emote_offset.min(MAX_CONTENT_BYTES),
        }
    }

    fn into_message(self) -> Option<ChatMessage> {
        if self.id.trim().is_empty()
            || self.channel.trim().is_empty()
            || self.sender.trim().is_empty()
        {
            return None;
        }
        let slots = Arc::from([]);
        Some(ChatMessage {
            id: Arc::from(cap_text(&self.id, MAX_FIELD_BYTES)),
            channel: Arc::from(cap_text(&self.channel, MAX_FIELD_BYTES)),
            emote_lookup_channel: self
                .emote_lookup_channel
                .map(|value| Arc::from(cap_text(&value, MAX_FIELD_BYTES))),
            emote_lookup_source_id: self
                .emote_lookup_source_id
                .map(|value| Arc::from(cap_text(&value, MAX_FIELD_BYTES))),
            sender: Arc::from(cap_text(&self.sender, MAX_FIELD_BYTES)),
            display_name: Arc::from(cap_text(&self.display_name, MAX_FIELD_BYTES)),
            content: Arc::from(cap_text(&self.content, MAX_CONTENT_BYTES)),
            color: self
                .color
                .map(|value| Arc::from(cap_text(&value, MAX_FIELD_BYTES))),
            badges: self
                .badges
                .into_iter()
                .take(MAX_BADGES)
                .map(|badge| Arc::from(cap_text(&badge, MAX_FIELD_BYTES)))
                .collect::<SmallVec<[Arc<str>; 4]>>(),
            timestamp: self.timestamp,
            slots,
            structure_hash: 0,
            reply: self.reply.and_then(StoredReply::into_reply),
            flags: self.flags.into_flags(),
            twitch_gifs_tag: self
                .twitch_gifs_tag
                .map(|value| Arc::from(cap_text(&value, MAX_CONTENT_BYTES))),
            twitch_emotes_tag: self
                .twitch_emotes_tag
                .map(|value| Arc::from(cap_text(&value, MAX_CONTENT_BYTES))),
            user_id: self
                .user_id
                .map(|value| Arc::from(cap_text(&value, MAX_FIELD_BYTES))),
            reply_emote_offset: self.reply_emote_offset.min(MAX_CONTENT_BYTES),
        })
    }
}

impl StoredReply {
    fn from_reply(reply: &ReplyInfo) -> Self {
        Self {
            parent_msg_id: cap_text(&reply.parent_msg_id, MAX_FIELD_BYTES),
            parent_user_login: cap_text(&reply.parent_user_login, MAX_FIELD_BYTES),
            parent_display_name: cap_text(&reply.parent_display_name, MAX_FIELD_BYTES),
            parent_user_id: cap_optional(reply.parent_user_id.as_deref(), MAX_FIELD_BYTES),
            parent_msg_body: cap_optional(reply.parent_msg_body.as_deref(), MAX_CONTENT_BYTES),
        }
    }

    fn into_reply(self) -> Option<ReplyInfo> {
        if self.parent_msg_id.trim().is_empty() || self.parent_user_login.trim().is_empty() {
            return None;
        }
        Some(ReplyInfo {
            parent_msg_id: Arc::from(cap_text(&self.parent_msg_id, MAX_FIELD_BYTES)),
            parent_user_login: Arc::from(cap_text(&self.parent_user_login, MAX_FIELD_BYTES)),
            parent_display_name: Arc::from(cap_text(&self.parent_display_name, MAX_FIELD_BYTES)),
            parent_user_id: self
                .parent_user_id
                .map(|value| Arc::from(cap_text(&value, MAX_FIELD_BYTES))),
            parent_msg_body: self
                .parent_msg_body
                .map(|value| Arc::from(cap_text(&value, MAX_CONTENT_BYTES))),
        })
    }
}

impl StoredFlags {
    fn from_flags(flags: &MessageFlags) -> Self {
        Self {
            is_action: flags.is_action,
            highlight_type: flags.highlight_type.clone(),
            is_system: flags.is_system,
            is_usernotice: flags.is_usernotice,
            is_first_message: flags.is_first_message,
            system_type: flags.system_type.clone(),
            is_shared_chat: flags.is_shared_chat,
            is_deleted: flags.is_deleted,
            is_elevated_message: flags.is_elevated_message,
            is_hype_chat: flags.is_hype_chat,
            is_automod: flags.is_automod,
            is_automod_pending: flags.is_automod_pending,
            is_suspicious_user: flags.is_suspicious_user,
            is_restricted: flags.is_restricted,
            is_monitored: flags.is_monitored,
            is_similar: flags.is_similar,
            is_alternate_row: flags.is_alternate_row,
        }
    }

    fn into_flags(self) -> MessageFlags {
        MessageFlags {
            is_action: self.is_action,
            highlight_type: self.highlight_type,
            is_system: self.is_system,
            is_usernotice: self.is_usernotice,
            is_first_message: self.is_first_message,
            system_type: self.system_type,
            is_shared_chat: self.is_shared_chat,
            is_deleted: self.is_deleted,
            deleted_presentation: Default::default(),
            is_elevated_message: self.is_elevated_message,
            is_hype_chat: self.is_hype_chat,
            is_automod: self.is_automod,
            is_automod_pending: self.is_automod_pending,
            is_suspicious_user: self.is_suspicious_user,
            is_restricted: self.is_restricted,
            is_monitored: self.is_monitored,
            is_similar: self.is_similar,
            is_alternate_row: self.is_alternate_row,
        }
    }
}
