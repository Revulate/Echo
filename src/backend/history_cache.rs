//! Local message history cache for offline/fallback message persistence.
//!
//! Stores per-channel message history as Postcard + zstd compressed files.
//! Legacy Bincode snapshots remain readable for migration compatibility.
//! Uses a lightweight `StoredMessage` DTO to avoid serializing complex Arc types.
//! On load, messages are reconstructed and slots retokenized.

use super::models::{ChatMessage, HighlightType, MessageFlags, ReplyInfo};
use super::slots::compute_structure_hash;
use super::slots::parser::parse_emotes_tag;
use crate::backend::lock_utils::MutexExt;
use bincode::Decode;
#[cfg(test)]
use bincode::Encode;
use chrono::{Local, TimeZone, Utc};
use dashmap::DashSet;
use rustc_hash::FxHashMap;
use serde::{Deserialize, Serialize};
use smallvec::SmallVec;
use std::fmt;
use std::io::{Cursor, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, LazyLock, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant, SystemTime};

mod legacy;
mod readable_logs;

use legacy::*;
pub use readable_logs::*;

/// Maximum messages to store per channel
const MAX_STORED_MESSAGES: usize = 200;
/// History snapshots are generated locally from at most 200 bounded chat rows.
/// These deliberately generous ceilings keep existing caches compatible while
/// preventing a corrupt or replaced file from allocating without limit.
const MAX_HISTORY_CACHE_COMPRESSED_BYTES: u64 = 8 * 1024 * 1024;
const MAX_HISTORY_CACHE_DECOMPRESSED_BYTES: usize = 32 * 1024 * 1024;
/// Prefix for the history-cache format that persists reply-parent user IDs.
const HISTORY_CACHE_V2_MAGIC: &[u8; 8] = b"ECHOHC2\0";
/// Prefix for parse-time third-party emote lookup-scope persistence.
const HISTORY_CACHE_V3_MAGIC: &[u8; 8] = b"ECHOHC3\0";
/// Prefix for the original Postcard-backed history-cache format.
const HISTORY_CACHE_V4_MAGIC: &[u8; 8] = b"ECHOHC4\0";
/// Current Postcard format, preserving Twitch GIF annotations.
const HISTORY_CACHE_V5_MAGIC: &[u8; 8] = b"ECHOHC5\0";

/// Failures while loading a locally generated history snapshot.
///
/// Filesystem failures are deliberately distinct from invalid bytes: a transient
/// open/read failure must not erase an otherwise recoverable cache file.
#[derive(Debug)]
enum HistoryCacheReadError {
    Io {
        operation: &'static str,
        source: std::io::Error,
    },
    CompressedSizeLimit {
        observed_bytes: u64,
        limit_bytes: u64,
    },
    DecompressedSizeLimit {
        limit_bytes: usize,
    },
    InvalidSnapshot {
        stage: &'static str,
        detail: String,
    },
}

impl HistoryCacheReadError {
    fn io(operation: &'static str, source: std::io::Error) -> Self {
        Self::Io { operation, source }
    }

    fn invalid_snapshot(stage: &'static str, error: impl fmt::Display) -> Self {
        Self::InvalidSnapshot {
            stage,
            detail: error.to_string(),
        }
    }
}

impl fmt::Display for HistoryCacheReadError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { operation, source } => {
                write!(
                    formatter,
                    "failed to {operation} history cache file: {source}"
                )
            }
            Self::CompressedSizeLimit {
                observed_bytes,
                limit_bytes,
            } => write!(
                formatter,
                "history cache file is too large ({observed_bytes} bytes observed; maximum is {limit_bytes})"
            ),
            Self::DecompressedSizeLimit { limit_bytes } => write!(
                formatter,
                "decompressed history cache exceeds {limit_bytes} bytes"
            ),
            Self::InvalidSnapshot { stage, detail } => {
                write!(formatter, "invalid history cache {stage}: {detail}")
            }
        }
    }
}

impl std::error::Error for HistoryCacheReadError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::CompressedSizeLimit { .. }
            | Self::DecompressedSizeLimit { .. }
            | Self::InvalidSnapshot { .. } => None,
        }
    }
}

/// New saves after a clear action only persist messages newer than this cutoff.
static HISTORY_CACHE_CLEAR_AFTER_MS: AtomicI64 = AtomicI64::new(0);
static HISTORY_CACHE_IO_LOCK: Mutex<()> = Mutex::new(());

/// Lightweight DTO for serialization — no Arc, no complex types
#[derive(Serialize, Deserialize, Clone)]
struct StoredMessage {
    id: String,
    sender: String,
    display_name: String,
    content: String,
    emote_lookup_channel: Option<String>,
    emote_lookup_source_id: Option<String>,
    color: Option<String>,
    badges: Vec<String>,
    timestamp: i64,
    user_id: Option<String>,
    twitch_emotes_tag: Option<String>,
    reply_emote_offset: usize,
    is_action: bool,
    reply_parent_msg_id: Option<String>,
    reply_parent_user_login: Option<String>,
    reply_parent_display_name: Option<String>,
    reply_parent_msg_body: Option<String>,
    reply_parent_user_id: Option<String>,
    twitch_gifs_tag: Option<String>,
}

impl StoredMessage {
    /// Convert a ChatMessage into a StoredMessage for serialization
    fn from_chat_message(msg: &ChatMessage) -> Self {
        Self {
            twitch_gifs_tag: msg.twitch_gifs_tag.as_ref().map(|tag| tag.to_string()),
            id: msg.id.to_string(),
            sender: msg.sender.to_string(),
            display_name: msg.display_name.to_string(),
            content: msg.content.to_string(),
            emote_lookup_channel: msg
                .emote_lookup_channel
                .as_ref()
                .map(|channel| channel.to_string()),
            emote_lookup_source_id: msg.emote_lookup_source_id.as_ref().map(|id| id.to_string()),
            color: msg.color.as_ref().map(|c| c.to_string()),
            badges: msg.badges.iter().map(|b| b.to_string()).collect(),
            timestamp: msg.timestamp,
            user_id: msg.user_id.as_ref().map(|id| id.to_string()),
            twitch_emotes_tag: msg.twitch_emotes_tag.as_ref().map(|t| t.to_string()),
            reply_emote_offset: msg.reply_emote_offset,
            is_action: msg.flags.is_action,
            reply_parent_msg_id: msg.reply.as_ref().map(|r| r.parent_msg_id.to_string()),
            reply_parent_user_login: msg.reply.as_ref().map(|r| r.parent_user_login.to_string()),
            reply_parent_display_name: msg
                .reply
                .as_ref()
                .map(|r| r.parent_display_name.to_string()),
            reply_parent_msg_body: msg
                .reply
                .as_ref()
                .and_then(|r| r.parent_msg_body.as_ref().map(|b| b.to_string())),
            reply_parent_user_id: msg
                .reply
                .as_ref()
                .and_then(|r| r.parent_user_id.as_ref().map(|id| id.to_string())),
        }
    }

    /// Reconstruct a ChatMessage from stored data, retokenizing slots
    fn into_chat_message(self, channel: &str) -> ChatMessage {
        let twitch_emotes: FxHashMap<usize, (String, usize)> = self
            .twitch_emotes_tag
            .as_deref()
            .map(parse_emotes_tag)
            .unwrap_or_default()
            .into_iter()
            .filter_map(|(start, (id, end))| {
                if end <= self.reply_emote_offset {
                    return None;
                }
                let new_start = start.saturating_sub(self.reply_emote_offset);
                let new_end = end.saturating_sub(self.reply_emote_offset);
                if new_end <= new_start {
                    return None;
                }
                Some((new_start, (id, new_end)))
            })
            .collect();

        let stored_emote_lookup_channel = self.emote_lookup_channel.map(Arc::from);
        let stored_emote_lookup_source_id = self.emote_lookup_source_id.map(Arc::from);
        let emote_lookup_channel = stored_emote_lookup_channel.as_deref().unwrap_or(channel);
        let slots = crate::backend::slots::parser::parse_to_slots_with_gifs(
            &self.content,
            emote_lookup_channel,
            crate::backend::emotes::EmoteSender::new(self.user_id.as_deref(), &self.sender),
            &twitch_emotes,
            self.twitch_gifs_tag.as_deref(),
            self.reply_emote_offset,
        );
        let structure_hash = compute_structure_hash(&slots);
        let slots: Arc<[_]> = slots.into();

        let reply = if let (Some(parent_id), Some(parent_user)) =
            (self.reply_parent_msg_id, self.reply_parent_user_login)
        {
            Some(ReplyInfo {
                parent_msg_id: Arc::from(parent_id),
                parent_user_login: Arc::from(parent_user.clone()),
                parent_display_name: Arc::from(
                    self.reply_parent_display_name.unwrap_or(parent_user),
                ),
                parent_user_id: self.reply_parent_user_id.map(Arc::from),
                parent_msg_body: self.reply_parent_msg_body.map(Arc::from),
            })
        } else {
            None
        };

        let badges: SmallVec<[Arc<str>; 4]> = self.badges.into_iter().map(Arc::from).collect();

        let username = super::settings::get_username_lowercase();
        let is_mention = !username.is_empty()
            && !self.sender.eq_ignore_ascii_case(&username)
            && super::models::contains_ascii_case_insensitive(&self.content, &username);
        let highlight_type = if is_mention {
            Some(HighlightType::Mention)
        } else {
            None
        };

        let is_usernotice = self.id.ends_with(":content");
        ChatMessage {
            id: Arc::from(self.id),
            channel: Arc::from(channel),
            emote_lookup_channel: stored_emote_lookup_channel,
            emote_lookup_source_id: stored_emote_lookup_source_id,
            sender: Arc::from(self.sender),
            display_name: Arc::from(self.display_name),
            content: Arc::from(self.content),
            color: self.color.map(|c| Arc::from(c.as_str())),
            badges,
            timestamp: self.timestamp,
            slots,
            structure_hash,
            reply,
            flags: MessageFlags {
                is_action: self.is_action,
                highlight_type,
                is_system: false,
                is_usernotice,
                is_first_message: false,
                system_type: None,
                is_shared_chat: false,
                is_deleted: false,
                deleted_presentation: Default::default(),
                is_elevated_message: false,
                is_hype_chat: false,
                is_automod: false,
                is_automod_pending: false,
                is_suspicious_user: false,
                is_restricted: false,
                is_monitored: false,
                is_similar: false,
                is_alternate_row: false,
            },
            twitch_gifs_tag: self.twitch_gifs_tag.map(Arc::from),
            twitch_emotes_tag: self.twitch_emotes_tag.map(|t| Arc::from(t.as_str())),
            user_id: self.user_id.map(|id| Arc::from(id.as_str())),
            reply_emote_offset: self.reply_emote_offset,
        }
    }
}

fn is_legacy_protocol_status_message(message: &StoredMessage) -> bool {
    super::history_filter::is_legacy_protocol_status_row(
        &message.sender,
        &message.display_name,
        message.user_id.is_some(),
        !message.badges.is_empty(),
        message.twitch_emotes_tag.is_some(),
        &message.content,
    )
}

/// Benchmark fixture rows from versions that allowed replay persistence.
///
/// This intentionally examines the stable synthetic message ID only: sender
/// names are user-controlled and must not affect history retention.
fn is_synthetic_replay_message_id(message_id: &str) -> bool {
    message_id.starts_with("echo-bench-") || message_id.starts_with("echo-replay-")
}

/// Synthetic replay rows and local-echo rows (`optimistic-…` ids) written by
/// older builds are not real Twitch messages and are dropped on load.
fn is_legacy_synthetic_message(message: &StoredMessage) -> bool {
    is_synthetic_replay_message_id(&message.id) || message.id.starts_with("optimistic-")
}

/// Moderator-removed rows keep their text in memory so a split can reveal them,
/// but the disk cache stores no flag for it: persisting one would bring the
/// removed message back as an ordinary row after a restart.
fn should_persist_history_message(message: &ChatMessage) -> bool {
    !message.flags.is_system
        && !message.flags.is_deleted
        && !crate::backend::whispers::is_whispers_channel(&message.channel)
        && !is_synthetic_replay_message_id(message.id.as_ref())
}

const LIVE_HISTORY_FLUSH_INTERVAL: Duration = Duration::from_secs(30);

static LIVE_HISTORY_DIRTY_CHANNELS: LazyLock<Arc<DashSet<String>>> =
    LazyLock::new(|| Arc::new(DashSet::new()));
static LIVE_HISTORY_TX: OnceLock<crossbeam_channel::Sender<LiveHistoryJob>> = OnceLock::new();
static LIVE_HISTORY_INTAKE_OPEN: AtomicBool = AtomicBool::new(true);
static LIVE_HISTORY_SEND_WARN_EMITTED: AtomicBool = AtomicBool::new(false);

enum LiveHistoryJob {
    Wake,
    Shutdown(std::sync::mpsc::Sender<()>),
}

fn live_history_tx() -> &'static crossbeam_channel::Sender<LiveHistoryJob> {
    LIVE_HISTORY_TX.get_or_init(|| {
        let (tx, rx) = crossbeam_channel::bounded(1);
        let dirty_channels = Arc::clone(&LIVE_HISTORY_DIRTY_CHANNELS);
        if let Err(error) = std::thread::Builder::new()
            .name("live-history-cache".into())
            .spawn(move || {
                live_history_writer_loop(
                    dirty_channels,
                    rx,
                    LIVE_HISTORY_FLUSH_INTERVAL,
                    persist_live_history_channels,
                )
            })
        {
            crate::log_net!(warn, "Failed to spawn live history cache writer: {}", error);
        }
        tx
    })
}

fn mark_live_history_channel_dirty(dirty_channels: &DashSet<String>, channel: &str) -> bool {
    let channel = channel.trim().trim_start_matches('#');
    !channel.is_empty() && dirty_channels.insert(channel.to_ascii_lowercase())
}

fn take_dirty_live_history_channels(dirty_channels: &DashSet<String>) -> Vec<String> {
    let channels: Vec<String> = dirty_channels
        .iter()
        .map(|channel| channel.key().clone())
        .collect();
    for channel in &channels {
        dirty_channels.remove(channel);
    }
    channels
}

fn live_history_writer_loop<F>(
    dirty_channels: Arc<DashSet<String>>,
    rx: crossbeam_channel::Receiver<LiveHistoryJob>,
    flush_interval: Duration,
    mut persist: F,
) where
    F: FnMut(Vec<String>),
{
    let mut deadline = Instant::now() + flush_interval;
    loop {
        let wait = deadline.saturating_duration_since(Instant::now());
        match rx.recv_timeout(wait) {
            Ok(LiveHistoryJob::Wake) | Err(crossbeam_channel::RecvTimeoutError::Timeout) => {}
            Ok(LiveHistoryJob::Shutdown(ack)) => {
                persist_dirty_live_history_channels(&dirty_channels, &mut persist);
                let _ = ack.send(());
                return;
            }
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => {
                persist_dirty_live_history_channels(&dirty_channels, &mut persist);
                return;
            }
        }

        if Instant::now() >= deadline {
            persist_dirty_live_history_channels(&dirty_channels, &mut persist);
            deadline = Instant::now() + flush_interval;
        }
    }
}

fn persist_dirty_live_history_channels<F>(dirty_channels: &DashSet<String>, persist: &mut F)
where
    F: FnMut(Vec<String>),
{
    let channels = take_dirty_live_history_channels(dirty_channels);
    if !channels.is_empty() {
        persist(channels);
    }
}

fn persist_live_history_channels(channels: Vec<String>) {
    use super::irc::CHAT_MESSAGES;

    for channel in channels {
        let Some(buffer) = CHAT_MESSAGES.get(&channel) else {
            continue;
        };
        let messages: Vec<ChatMessage> = buffer.iter().cloned().collect();
        drop(buffer);
        save_channel_history(&channel, &messages);
    }
}

pub(crate) fn schedule_visible_channel_history_persist(channel: &str) {
    if !LIVE_HISTORY_INTAKE_OPEN.load(Ordering::Acquire)
        || !super::settings::get_persist_history_enabled()
        || super::chat_replay::suppresses_history_persistence()
        || !mark_live_history_channel_dirty(&LIVE_HISTORY_DIRTY_CHANNELS, channel)
    {
        return;
    }

    match live_history_tx().try_send(LiveHistoryJob::Wake) {
        Ok(()) | Err(crossbeam_channel::TrySendError::Full(_)) => {}
        Err(crossbeam_channel::TrySendError::Disconnected(_)) => {
            if !LIVE_HISTORY_SEND_WARN_EMITTED.swap(true, Ordering::Relaxed) {
                crate::log_net!(
                    warn,
                    "live history persistence disabled: writer unavailable"
                );
            }
        }
    }
}

/// Stop accepting live-history work, drain dirty channels, and wait at most
/// `timeout` for the writer to acknowledge completion.
pub(crate) fn shutdown_live_history_writer(timeout: Duration) -> bool {
    if !LIVE_HISTORY_INTAKE_OPEN.swap(false, Ordering::AcqRel) {
        return true;
    }
    let Some(tx) = LIVE_HISTORY_TX.get() else {
        return true;
    };
    let started = Instant::now();
    let (ack_tx, ack_rx) = std::sync::mpsc::channel();
    match tx.send_timeout(LiveHistoryJob::Shutdown(ack_tx), timeout) {
        Ok(()) => {}
        Err(crossbeam_channel::SendTimeoutError::Timeout(_)) => {
            crate::log_net!(warn, "Timed out requesting live history cache shutdown");
            return false;
        }
        Err(crossbeam_channel::SendTimeoutError::Disconnected(_)) => {
            crate::log_net!(warn, "Live history cache writer stopped before shutdown");
            return false;
        }
    }
    match ack_rx.recv_timeout(timeout.saturating_sub(started.elapsed())) {
        Ok(()) => true,
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
            crate::log_net!(
                warn,
                "Timed out draining live history cache during shutdown"
            );
            false
        }
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
            crate::log_net!(
                warn,
                "Live history cache writer stopped before acknowledging shutdown"
            );
            false
        }
    }
}

/// Record one newly inserted live row for bounded binary persistence and the
/// opt-in human-readable channel log.
///
/// Both paths are write-behind; the caller never touches the filesystem.
pub fn record_live_chat_message(channel: &str, message: &ChatMessage) {
    if should_persist_history_message(message) {
        schedule_visible_channel_history_persist(channel);
    }

    if !super::settings::get_chat_logging_enabled() {
        return;
    }
    if message.flags.is_deleted && !super::settings::get_log_deleted_messages() {
        return;
    }
    let Some(_intake) = begin_readable_log_intake() else {
        return;
    };

    let channel = sanitize_export_file_stem(channel.trim_start_matches('#'));
    let line = format_readable_log_line(message);
    let day = readable_log_day(message.timestamp);
    send_readable_log_job(ReadableLogJob::Append { channel, day, line });
}

fn read_stored_history_file(path: &Path) -> Result<Vec<StoredMessage>, HistoryCacheReadError> {
    let decompressed = read_bounded_history_payload(path)?;
    decode_stored_messages(&decompressed)
}

fn read_bounded_history_payload(path: &Path) -> Result<Vec<u8>, HistoryCacheReadError> {
    let file =
        std::fs::File::open(path).map_err(|error| HistoryCacheReadError::io("open", error))?;
    let compressed_bytes = file
        .metadata()
        .map_err(|error| HistoryCacheReadError::io("inspect", error))?
        .len();
    if compressed_bytes > MAX_HISTORY_CACHE_COMPRESSED_BYTES {
        return Err(HistoryCacheReadError::CompressedSizeLimit {
            observed_bytes: compressed_bytes,
            limit_bytes: MAX_HISTORY_CACHE_COMPRESSED_BYTES,
        });
    }

    let mut compressed = Vec::new();
    file.take(MAX_HISTORY_CACHE_COMPRESSED_BYTES + 1)
        .read_to_end(&mut compressed)
        .map_err(|error| HistoryCacheReadError::io("read", error))?;
    if compressed.len() as u64 > MAX_HISTORY_CACHE_COMPRESSED_BYTES {
        return Err(HistoryCacheReadError::CompressedSizeLimit {
            observed_bytes: compressed.len() as u64,
            limit_bytes: MAX_HISTORY_CACHE_COMPRESSED_BYTES,
        });
    }

    decode_bounded_history_payload(&compressed, MAX_HISTORY_CACHE_DECOMPRESSED_BYTES)
}

fn decode_bounded_history_payload(
    compressed: &[u8],
    max_decompressed_bytes: usize,
) -> Result<Vec<u8>, HistoryCacheReadError> {
    let mut decoder = zstd::stream::read::Decoder::new(Cursor::new(compressed))
        .map_err(|error| HistoryCacheReadError::invalid_snapshot("compressed payload", error))?;
    decoder
        .window_log_max(25)
        .map_err(|error| HistoryCacheReadError::invalid_snapshot("compressed payload", error))?;
    let decompressed_capture_limit = u64::try_from(max_decompressed_bytes)
        .unwrap_or(u64::MAX)
        .saturating_add(1);
    let mut limited = decoder.take(decompressed_capture_limit);
    let mut decompressed = Vec::new();
    limited
        .read_to_end(&mut decompressed)
        .map_err(|error| HistoryCacheReadError::invalid_snapshot("compressed payload", error))?;
    if decompressed.len() > max_decompressed_bytes {
        return Err(HistoryCacheReadError::DecompressedSizeLimit {
            limit_bytes: max_decompressed_bytes,
        });
    }
    Ok(decompressed)
}

fn encode_stored_messages(messages: &[StoredMessage]) -> Result<Vec<u8>, postcard::Error> {
    let payload = postcard::to_stdvec(messages)?;
    let mut versioned = Vec::with_capacity(HISTORY_CACHE_V5_MAGIC.len() + payload.len());
    versioned.extend_from_slice(HISTORY_CACHE_V5_MAGIC);
    versioned.extend_from_slice(&payload);
    Ok(versioned)
}

fn trailing_payload_error(stage: &'static str, remaining_bytes: usize) -> HistoryCacheReadError {
    HistoryCacheReadError::InvalidSnapshot {
        stage,
        detail: format!(
            "contains {remaining_bytes} trailing byte{}",
            if remaining_bytes == 1 { "" } else { "s" }
        ),
    }
}

fn decode_legacy_payload<T>(payload: &[u8], stage: &'static str) -> Result<T, HistoryCacheReadError>
where
    T: Decode<()>,
{
    let config =
        bincode::config::standard().with_limit::<{ MAX_HISTORY_CACHE_DECOMPRESSED_BYTES }>();
    let (value, consumed) = bincode::decode_from_slice(payload, config)
        .map_err(|error| HistoryCacheReadError::invalid_snapshot(stage, error))?;
    if consumed > payload.len() {
        return Err(HistoryCacheReadError::InvalidSnapshot {
            stage,
            detail: format!(
                "decoder reported consuming {consumed} bytes from a {} byte payload",
                payload.len()
            ),
        });
    }
    if consumed != payload.len() {
        return Err(trailing_payload_error(stage, payload.len() - consumed));
    }
    Ok(value)
}

fn decode_stored_messages(bytes: &[u8]) -> Result<Vec<StoredMessage>, HistoryCacheReadError> {
    let messages = if let Some(payload) = bytes.strip_prefix(HISTORY_CACHE_V5_MAGIC) {
        let (messages, remaining) = postcard::take_from_bytes::<Vec<StoredMessage>>(payload)
            .map_err(|error| HistoryCacheReadError::invalid_snapshot("V5 payload", error))?;
        if !remaining.is_empty() {
            return Err(trailing_payload_error("V5 payload", remaining.len()));
        }
        messages
    } else if let Some(payload) = bytes.strip_prefix(HISTORY_CACHE_V4_MAGIC) {
        let (messages, remaining) = postcard::take_from_bytes::<Vec<StoredMessageV4>>(payload)
            .map_err(|error| HistoryCacheReadError::invalid_snapshot("V4 payload", error))?;
        if !remaining.is_empty() {
            return Err(trailing_payload_error("V4 payload", remaining.len()));
        }
        messages.into_iter().map(StoredMessage::from).collect()
    } else if let Some(payload) = bytes.strip_prefix(HISTORY_CACHE_V3_MAGIC) {
        decode_legacy_payload::<Vec<StoredMessageV3>>(payload, "V3 payload")?
            .into_iter()
            .map(StoredMessage::from)
            .collect()
    } else if let Some(payload) = bytes.strip_prefix(HISTORY_CACHE_V2_MAGIC) {
        decode_legacy_payload::<Vec<StoredMessageV2>>(payload, "V2 payload")?
            .into_iter()
            .map(StoredMessage::from)
            .collect()
    } else {
        decode_legacy_payload::<Vec<StoredMessageV1>>(bytes, "V1 payload")?
            .into_iter()
            .map(StoredMessage::from)
            .collect()
    };
    if messages.len() > MAX_STORED_MESSAGES {
        return Err(HistoryCacheReadError::InvalidSnapshot {
            stage: "message list",
            detail: format!(
                "contains {} messages; maximum is {MAX_STORED_MESSAGES}",
                messages.len()
            ),
        });
    }
    Ok(messages)
}

/// Get the history cache directory, creating it if needed
fn get_history_dir() -> Option<PathBuf> {
    let history_dir = history_dir_path()?;
    if !history_dir.exists()
        && let Err(e) = std::fs::create_dir_all(&history_dir)
    {
        crate::log_net!(warn, "Failed to create history cache dir: {}", e);
        return None;
    }
    Some(history_dir)
}

fn history_cache_io_guard() -> MutexGuard<'static, ()> {
    HISTORY_CACHE_IO_LOCK.lock_or_recover()
}

fn remove_channel_history_snapshot(path: &Path, channel: &str) {
    match std::fs::remove_file(path) {
        Ok(()) => crate::log_net!(debug, "Removed empty history cache for {}", channel),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            crate::log_net!(warn, "Failed to remove history for {}: {}", channel, error);
        }
    }
}

fn log_history_cache_load_failure(channel: &str, error: &HistoryCacheReadError) {
    match error {
        HistoryCacheReadError::Io { .. } => crate::log_net!(
            warn,
            "History cache unavailable for {}; preserving snapshot: {}",
            channel,
            error
        ),
        HistoryCacheReadError::CompressedSizeLimit { .. }
        | HistoryCacheReadError::DecompressedSizeLimit { .. }
        | HistoryCacheReadError::InvalidSnapshot { .. } => crate::log_net!(
            warn,
            "Invalid history cache snapshot for {}; preserving it until the next save: {}",
            channel,
            error
        ),
    }
}

/// Save channel history to local cache file
/// `<history dir>/<channel><suffix>`, or `None` when `channel` is not a
/// Twitch login, so an untrusted name can never escape the history directory.
fn channel_history_path(history_dir: &Path, channel: &str, suffix: &str) -> Option<PathBuf> {
    if !crate::utils::channel_input::is_valid_twitch_login(&channel.to_ascii_lowercase()) {
        crate::log_net!(
            warn,
            "Skipping history cache for invalid channel name {:?}",
            channel
        );
        return None;
    }
    Some(history_dir.join(format!("{channel}{suffix}")))
}

pub fn save_channel_history(channel: &str, messages: &[ChatMessage]) {
    if !super::settings::get_persist_history_enabled()
        || super::chat_replay::suppresses_history_persistence()
    {
        return;
    }
    let _io_guard = history_cache_io_guard();
    let Some(history_dir) = get_history_dir() else {
        return;
    };
    let clear_after_ms = HISTORY_CACHE_CLEAR_AFTER_MS.load(Ordering::Relaxed);

    let stored: Vec<StoredMessage> = messages
        .iter()
        .filter(|m| should_persist_history_message(m))
        .filter(|m| clear_after_ms == 0 || m.timestamp > clear_after_ms)
        .rev()
        .take(MAX_STORED_MESSAGES)
        .map(StoredMessage::from_chat_message)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();

    let Some(path) = channel_history_path(&history_dir, channel, ".bin.zst") else {
        return;
    };
    if stored.is_empty() {
        remove_channel_history_snapshot(&path, channel);
        return;
    }

    let encoded = match encode_stored_messages(&stored) {
        Ok(data) => data,
        Err(e) => {
            crate::log_net!(warn, "Failed to encode history for {}: {}", channel, e);
            return;
        }
    };

    let compressed = match zstd::encode_all(encoded.as_slice(), 3) {
        Ok(data) => data,
        Err(e) => {
            crate::log_net!(warn, "Failed to compress history for {}: {}", channel, e);
            return;
        }
    };

    let Some(tmp_path) = channel_history_path(&history_dir, channel, ".bin.zst.tmp") else {
        return;
    };

    if let Err(e) = std::fs::write(&tmp_path, &compressed) {
        crate::log_net!(warn, "Failed to write history for {}: {}", channel, e);
        let _ = std::fs::remove_file(&tmp_path);
        return;
    }

    if let Err(e) = crate::utils::replace_file(&tmp_path, &path) {
        crate::log_net!(warn, "Failed to replace history for {}: {}", channel, e);
        let _ = std::fs::remove_file(&tmp_path);
        return;
    }

    crate::log_net!(
        debug,
        "Saved {} messages for {} ({} bytes compressed)",
        stored.len(),
        channel,
        compressed.len()
    );
}

/// Load channel history from local cache file
pub fn load_channel_history(channel: &str) -> Vec<ChatMessage> {
    let Some(history_dir) = get_history_dir() else {
        return vec![];
    };

    let Some(path) = channel_history_path(&history_dir, channel, ".bin.zst") else {
        return vec![];
    };
    if !path.exists() {
        return vec![];
    }

    let decompressed = match read_bounded_history_payload(&path) {
        Ok(data) => data,
        Err(error) => {
            log_history_cache_load_failure(channel, &error);
            return vec![];
        }
    };

    let stored = match decode_stored_messages(&decompressed) {
        Ok(data) => data,
        Err(error) => {
            log_history_cache_load_failure(channel, &error);
            return vec![];
        }
    };

    let stored_count = stored.len();
    let messages: Vec<ChatMessage> = stored
        .into_iter()
        .filter(|message| {
            !is_legacy_protocol_status_message(message) && !is_legacy_synthetic_message(message)
        })
        .map(|s| s.into_chat_message(channel))
        .collect();
    let count = messages.len();
    let filtered_count = stored_count.saturating_sub(count);

    if filtered_count > 0 {
        crate::log_net!(
            info,
            "Loaded {} cached messages for {} (filtered {} legacy status or replay rows)",
            count,
            channel,
            filtered_count
        );
    } else {
        crate::log_net!(info, "Loaded {} cached messages for {}", count, channel);
    }

    messages
}

/// Remove persisted local message history cache files.
pub fn clear_history_cache() -> Result<usize, String> {
    let _io_guard = history_cache_io_guard();
    HISTORY_CACHE_CLEAR_AFTER_MS.store(Utc::now().timestamp_millis(), Ordering::Relaxed);

    let Some(history_dir) = history_dir_path() else {
        return Ok(0);
    };

    if !history_dir.exists() {
        return Ok(0);
    }

    let entries = std::fs::read_dir(&history_dir)
        .map_err(|e| format!("Failed to read history cache directory: {e}"))?;
    let mut removed = 0usize;

    for entry in entries {
        let entry = entry.map_err(|e| format!("Failed to inspect history cache entry: {e}"))?;
        let path = entry.path();
        if !path.is_file() {
            continue;
        }

        let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };

        if file_name.ends_with(".bin.zst") || file_name.ends_with(".bin.zst.tmp") {
            std::fs::remove_file(&path)
                .map_err(|e| format!("Failed to remove {}: {e}", path.display()))?;
            removed += 1;
        }
    }

    crate::log_net!(info, "Cleared {} history cache files", removed);
    Ok(removed)
}

/// Save all channels' message history to disk (called on shutdown)
pub fn save_all_channels() {
    use super::irc::CHAT_MESSAGES;

    if !super::settings::get_persist_history_enabled() {
        return;
    }

    let channels: Vec<String> = CHAT_MESSAGES.iter().map(|r| r.key().clone()).collect();

    for channel in &channels {
        if let Some(buffer) = CHAT_MESSAGES.get(channel) {
            let messages: Vec<ChatMessage> = buffer.iter().cloned().collect();
            // Release the shard before compressing and writing, as the live
            // writer does, so late IRC producers and UI reads are not blocked.
            drop(buffer);
            save_channel_history(channel, &messages);
        }
    }

    crate::log_net!(info, "Saved history cache for {} channels", channels.len());
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test;

    #[test]
    fn history_paths_reject_non_login_channel_names() {
        let dir = Path::new("history");
        assert_eq!(
            channel_history_path(dir, "Forsen_1", ".bin.zst"),
            Some(dir.join("Forsen_1.bin.zst"))
        );
        for bad in [
            "",
            "../evil",
            "a/b",
            "a\\b",
            "c:x",
            "#forsen",
            &"a".repeat(26),
        ] {
            assert_eq!(channel_history_path(dir, bad, ".bin.zst"), None, "{bad}");
        }
    }

    #[test]
    fn readable_day_log_names_match_only_echo_daily_logs() {
        assert!(is_readable_day_log_file_name(
            "forsen",
            "forsen-2026-09-25.txt"
        ));
        assert!(is_readable_day_log_file_name(
            "forsen",
            "forsen-2026-09-25.LOG"
        ));
        assert!(!is_readable_day_log_file_name("forsen", "forsen.txt"));
        assert!(!is_readable_day_log_file_name(
            "forsen",
            "xqc-2026-09-25.txt"
        ));
        assert!(!is_readable_day_log_file_name(
            "forsen",
            "forsen-2026-13-01.txt"
        ));
        assert!(!is_readable_day_log_file_name("forsen", "forsen-notes.txt"));
        assert!(!is_readable_day_log_file_name(
            "forsen",
            "forsen-2026-09-25.md"
        ));
        assert!(!is_readable_day_log_file_name(
            "forsen",
            "forsen-2026-09-25"
        ));
    }

    #[test]
    fn day_log_writer_emits_the_header_clear_requires() {
        let directory = tempfile::tempdir().expect("temporary log directory should exist");
        let day = chrono::NaiveDate::from_ymd_opt(2026, 9, 25).expect("valid date");
        let path = readable_log_day_path(directory.path(), "forsen", "txt", day);
        drop(open_readable_log_day_file(&path, "forsen").expect("day log should open"));
        assert!(file_starts_with(&path, READABLE_LOG_HEADER_PREFIX));

        let foreign = path.with_file_name("forsen-2026-09-24.txt");
        std::fs::write(&foreign, "user notes\n").expect("foreign file should write");
        assert!(!file_starts_with(&foreign, READABLE_LOG_HEADER_PREFIX));
    }

    #[test]
    fn history_export_names_match_only_sanitized_stems() {
        assert!(is_history_export_file_name("forsen.txt"));
        assert!(is_history_export_file_name("some_chan-1.txt"));
        assert!(!is_history_export_file_name(".txt"));
        assert!(!is_history_export_file_name("my notes.txt"));
        assert!(!is_history_export_file_name("forsen.log"));
    }

    #[test]
    fn readable_log_in_place_redaction_keeps_byte_length() {
        let line = "[2026-09-23 10:00:00] msg_id=abc <User> hello there";
        let redacted = redacted_readable_log_line_in_place(line, "msg_id=abc").unwrap();
        assert_eq!(redacted.len(), line.len());
        assert!(redacted.starts_with("[2026-09-23 10:00:00] msg_id=abc [redacted"));
        assert!(!redacted.contains("hello"));
        assert!(redacted_readable_log_line_in_place(line, "msg_id=ab").is_none());
    }

    #[test]
    fn readable_log_tail_scan_redacts_only_the_matching_whole_row() {
        let text = concat!(
            "ial row msg_id=abc tail\n",
            "[t1] msg_id=abc <A> first secret\n",
            "[t2] msg_id=abcd <B> other\n",
            "[t3] msg_id=abc [deleted] sender=a\n",
        );
        let (offset, redacted) =
            find_redactable_readable_log_line(text.as_bytes(), true, "msg_id=abc").unwrap();
        let line = "[t1] msg_id=abc <A> first secret";
        assert_eq!(&text[offset..offset + line.len()], line);
        assert_eq!(redacted.len(), line.len());
        assert!(!redacted.contains("secret"));
        assert!(
            find_redactable_readable_log_line(redacted.as_bytes(), false, "msg_id=abc").is_none()
        );
        assert!(redactable_readable_log_line("[t] msg_id=abc <A> x\n[t", "msg_id=abc").is_none());
    }

    #[test]
    fn readable_log_channel_name_reads_daily_and_legacy_paths() {
        assert_eq!(
            channel_name_from_log_path(Path::new("logs/forsen/forsen-2026-09-23.log")).as_deref(),
            Some("forsen")
        );
        assert_eq!(
            channel_name_from_log_path(Path::new("logs/forsen.txt")).as_deref(),
            Some("forsen")
        );
    }

    #[test]
    fn live_history_dirty_channels_are_coalesced_per_flush_window() {
        let dirty_channels = DashSet::new();

        assert!(mark_live_history_channel_dirty(&dirty_channels, "#Example"));
        assert!(!mark_live_history_channel_dirty(&dirty_channels, "example"));

        assert_eq!(
            take_dirty_live_history_channels(&dirty_channels),
            vec!["example".to_string()]
        );
        assert!(dirty_channels.is_empty());
    }

    #[test]
    fn live_history_flush_deadline_is_not_postponed_by_continuous_wakes() {
        let dirty_channels = Arc::new(DashSet::new());
        assert!(mark_live_history_channel_dirty(
            &dirty_channels,
            "continuous"
        ));
        let (wake_tx, wake_rx) = crossbeam_channel::bounded(1);
        let (persisted_tx, persisted_rx) = crossbeam_channel::bounded(1);
        let worker_dirty_channels = Arc::clone(&dirty_channels);
        let worker = std::thread::spawn(move || {
            live_history_writer_loop(
                worker_dirty_channels,
                wake_rx,
                Duration::from_millis(25),
                move |channels| {
                    let _ = persisted_tx.send(channels);
                },
            );
        });
        let producer_tx = wake_tx.clone();
        let producer = std::thread::spawn(move || {
            let started = Instant::now();
            while started.elapsed() < Duration::from_millis(250) {
                let _ = producer_tx.try_send(LiveHistoryJob::Wake);
                std::thread::sleep(Duration::from_millis(1));
            }
        });

        let persisted = persisted_rx
            .recv_timeout(Duration::from_millis(175))
            .expect("fixed deadline should flush while wake traffic is still active");
        assert_eq!(persisted, vec!["continuous".to_string()]);

        producer.join().expect("wake producer should finish");
        drop(wake_tx);
        worker.join().expect("history writer should stop");
    }

    #[test]
    fn live_history_shutdown_flushes_dirty_channels() {
        let dirty_channels = Arc::new(DashSet::new());
        assert!(mark_live_history_channel_dirty(&dirty_channels, "shutdown"));
        let (job_tx, job_rx) = crossbeam_channel::bounded(1);
        let (persisted_tx, persisted_rx) = crossbeam_channel::bounded(1);
        let worker_dirty_channels = Arc::clone(&dirty_channels);
        let worker = std::thread::spawn(move || {
            live_history_writer_loop(
                worker_dirty_channels,
                job_rx,
                Duration::from_secs(60),
                move |channels| {
                    let _ = persisted_tx.send(channels);
                },
            );
        });
        let (ack_tx, ack_rx) = std::sync::mpsc::channel();

        job_tx
            .send(LiveHistoryJob::Shutdown(ack_tx))
            .expect("shutdown should reach the history writer");
        ack_rx
            .recv_timeout(Duration::from_millis(100))
            .expect("history writer should acknowledge shutdown");

        assert_eq!(
            persisted_rx
                .recv_timeout(Duration::from_millis(100))
                .expect("shutdown should flush dirty channels"),
            vec!["shutdown".to_string()]
        );
        worker.join().expect("history writer should stop");
    }

    #[test]
    fn live_history_empty_snapshot_removes_stale_file() {
        let directory = tempfile::tempdir().expect("temporary history directory should exist");
        let path = directory.path().join("example.bin.zst");
        std::fs::write(&path, b"stale").expect("stale history fixture should be written");

        remove_channel_history_snapshot(&path, "example");

        assert!(!path.exists());
    }

    fn compressed_history_fixture(payload: &[u8]) -> Vec<u8> {
        zstd::encode_all(payload, 3).expect("history fixture should compress")
    }

    #[test]
    fn history_cache_filesystem_failure_preserves_path() {
        let directory = tempfile::tempdir().expect("temporary history directory should exist");

        let error = read_bounded_history_payload(directory.path())
            .expect_err("a directory cannot be read as a history snapshot");

        assert!(matches!(&error, HistoryCacheReadError::Io { .. }));
        assert!(directory.path().exists());
    }

    #[test]
    fn history_cache_compressed_limit_preserves_sparse_snapshot() {
        let directory = tempfile::tempdir().expect("temporary history directory should exist");
        let path = directory.path().join("oversized.bin.zst");
        let file = std::fs::File::create(&path).expect("sparse history fixture should be created");
        file.set_len(MAX_HISTORY_CACHE_COMPRESSED_BYTES + 1)
            .expect("sparse history fixture should be sized");
        drop(file);

        let error = read_bounded_history_payload(&path)
            .expect_err("oversized sparse snapshots must exceed the compressed limit");
        assert!(matches!(
            &error,
            HistoryCacheReadError::CompressedSizeLimit { .. }
        ));
        assert!(path.exists());
    }

    #[test]
    fn history_cache_corrupt_zstd_preserves_snapshot() {
        let directory = tempfile::tempdir().expect("temporary history directory should exist");
        let path = directory.path().join("corrupt-zstd.bin.zst");
        std::fs::write(&path, b"not zstd").expect("corrupt history fixture should be written");

        let error = read_bounded_history_payload(&path)
            .expect_err("corrupt zstd bytes must be classified as invalid snapshots");
        assert!(matches!(
            &error,
            HistoryCacheReadError::InvalidSnapshot { .. }
        ));
        assert!(path.exists());
    }

    #[test]
    fn history_cache_small_decompression_limit_preserves_snapshot() {
        let directory = tempfile::tempdir().expect("temporary history directory should exist");
        let path = directory.path().join("decompression-limit.bin.zst");
        let compressed = compressed_history_fixture(b"small bounded fixture");
        std::fs::write(&path, &compressed).expect("compressed history fixture should be written");
        const SMALL_DECOMPRESSED_LIMIT: usize = 4;

        let error = decode_bounded_history_payload(&compressed, SMALL_DECOMPRESSED_LIMIT)
            .expect_err("small fixture must exceed the test decompression limit");
        assert!(matches!(
            &error,
            HistoryCacheReadError::DecompressedSizeLimit {
                limit_bytes: SMALL_DECOMPRESSED_LIMIT
            }
        ));
        assert!(path.exists());
    }

    #[test]
    fn history_cache_valid_zstd_with_corrupt_bincode_preserves_snapshot() {
        let directory = tempfile::tempdir().expect("temporary history directory should exist");
        let path = directory.path().join("corrupt-bincode.bin.zst");
        std::fs::write(
            &path,
            compressed_history_fixture(b"not a bincode history payload"),
        )
        .expect("valid zstd fixture should be written");

        let decompressed = read_bounded_history_payload(&path)
            .expect("valid zstd fixture should decompress before bincode validation");
        let error = decode_stored_messages(&decompressed)
            .map(|messages| messages.len())
            .expect_err("corrupt bincode must be classified as an invalid snapshot");
        assert!(matches!(
            &error,
            HistoryCacheReadError::InvalidSnapshot { .. }
        ));
        assert!(path.exists());
    }

    fn stored_message_fixture() -> StoredMessage {
        StoredMessage {
            twitch_gifs_tag: None,
            id: "postcard-v4".to_string(),
            sender: "sender".to_string(),
            display_name: "Sender".to_string(),
            content: "plain".to_string(),
            emote_lookup_channel: Some("source".to_string()),
            emote_lookup_source_id: Some("source-id".to_string()),
            color: Some("#abcdef".to_string()),
            badges: vec!["subscriber".to_string()],
            timestamp: 1,
            user_id: Some("123".to_string()),
            twitch_emotes_tag: Some("25:0-4".to_string()),
            reply_emote_offset: 0,
            is_action: false,
            reply_parent_msg_id: Some("parent".to_string()),
            reply_parent_user_login: Some("parent-login".to_string()),
            reply_parent_display_name: Some("Parent".to_string()),
            reply_parent_msg_body: Some("parent body".to_string()),
            reply_parent_user_id: Some("456".to_string()),
        }
    }

    fn stored_message_v3_fixture() -> StoredMessageV3 {
        StoredMessageV3 {
            id: "legacy-v3".to_string(),
            sender: "sender".to_string(),
            display_name: "Sender".to_string(),
            content: "plain".to_string(),
            emote_lookup_channel: Some("source".to_string()),
            emote_lookup_source_id: Some("source-id".to_string()),
            color: Some("#abcdef".to_string()),
            badges: vec!["subscriber".to_string()],
            timestamp: 1,
            user_id: Some("123".to_string()),
            twitch_emotes_tag: Some("25:0-4".to_string()),
            reply_emote_offset: 0,
            is_action: false,
            reply_parent_msg_id: Some("parent".to_string()),
            reply_parent_user_login: Some("parent-login".to_string()),
            reply_parent_display_name: Some("Parent".to_string()),
            reply_parent_msg_body: Some("parent body".to_string()),
            reply_parent_user_id: Some("456".to_string()),
        }
    }

    fn stored_message_v1_fixture() -> StoredMessageV1 {
        StoredMessageV1 {
            id: "legacy-v1".to_string(),
            sender: "sender".to_string(),
            display_name: "Sender".to_string(),
            content: "plain".to_string(),
            color: None,
            badges: Vec::new(),
            timestamp: 1,
            user_id: None,
            twitch_emotes_tag: None,
            reply_emote_offset: 0,
            is_action: false,
            reply_parent_msg_id: None,
            reply_parent_user_login: None,
            reply_parent_display_name: None,
            reply_parent_msg_body: None,
        }
    }

    fn stored_message_v2_fixture() -> StoredMessageV2 {
        StoredMessageV2 {
            id: "legacy-v2".to_string(),
            sender: "sender".to_string(),
            display_name: "Sender".to_string(),
            content: "plain".to_string(),
            color: None,
            badges: Vec::new(),
            timestamp: 1,
            user_id: None,
            twitch_emotes_tag: None,
            reply_emote_offset: 0,
            is_action: false,
            reply_parent_msg_id: None,
            reply_parent_user_login: None,
            reply_parent_display_name: None,
            reply_parent_msg_body: None,
            reply_parent_user_id: None,
        }
    }

    #[test]
    fn history_v4_round_trip_preserves_current_message_fields() {
        let stored = vec![stored_message_fixture()];
        let payload = encode_stored_messages(&stored).expect("v4 history should encode");
        assert!(payload.starts_with(HISTORY_CACHE_V5_MAGIC));

        let restored = decode_stored_messages(&payload).expect("v4 history should decode");
        assert_eq!(restored.len(), 1);
        let restored = &restored[0];
        assert_eq!(restored.id, stored[0].id);
        assert_eq!(
            restored.emote_lookup_channel,
            stored[0].emote_lookup_channel
        );
        assert_eq!(
            restored.emote_lookup_source_id,
            stored[0].emote_lookup_source_id
        );
        assert_eq!(
            restored.reply_parent_user_id,
            stored[0].reply_parent_user_id
        );
    }

    #[test]
    fn history_v5_rejects_trailing_payload_bytes() {
        let mut payload = encode_stored_messages(&[stored_message_fixture()])
            .expect("v5 history fixture should encode");
        payload.push(0);

        let error = decode_stored_messages(&payload)
            .map(|messages| messages.len())
            .expect_err("v5 history must consume the complete payload");
        assert!(matches!(
            error,
            HistoryCacheReadError::InvalidSnapshot {
                stage: "V5 payload",
                ..
            }
        ));
    }

    #[test]
    fn history_v4_rejects_corrupt_payload() {
        let mut payload = HISTORY_CACHE_V4_MAGIC.to_vec();
        payload.push(0x80);

        let error = decode_stored_messages(&payload)
            .map(|messages| messages.len())
            .expect_err("truncated postcard payload must be classified as invalid");
        assert!(matches!(
            error,
            HistoryCacheReadError::InvalidSnapshot {
                stage: "V4 payload",
                ..
            }
        ));
    }

    #[test]
    fn history_v3_preserves_shared_emote_lookup_scope() {
        let stored = vec![stored_message_v3_fixture()];
        let encoded = bincode::encode_to_vec(&stored, bincode::config::standard())
            .expect("v3 history should encode");
        let mut payload = HISTORY_CACHE_V3_MAGIC.to_vec();
        payload.extend(encoded);

        let restored = decode_stored_messages(&payload)
            .expect("v3 history should decode")
            .pop()
            .expect("one row")
            .into_chat_message("host");

        assert_eq!(restored.emote_lookup_channel.as_deref(), Some("source"));
        assert_eq!(
            restored.emote_lookup_source_id.as_deref(),
            Some("source-id")
        );
    }

    #[test]
    fn history_v3_rejects_trailing_payload_bytes() {
        let encoded =
            bincode::encode_to_vec(&[stored_message_v3_fixture()], bincode::config::standard())
                .expect("v3 history fixture should encode");
        let mut payload = HISTORY_CACHE_V3_MAGIC.to_vec();
        payload.extend(encoded);
        payload.push(0);

        let error = decode_stored_messages(&payload)
            .map(|messages| messages.len())
            .expect_err("v3 history must consume the complete payload");
        assert!(matches!(
            error,
            HistoryCacheReadError::InvalidSnapshot {
                stage: "V3 payload",
                ..
            }
        ));
    }

    #[test]
    fn history_v1_defaults_newer_fields() {
        let stored = vec![stored_message_v1_fixture()];
        let payload = bincode::encode_to_vec(&stored, bincode::config::standard())
            .expect("v1 history fixture should encode");

        let restored = decode_stored_messages(&payload)
            .expect("v1 history should decode")
            .pop()
            .expect("one row");

        assert_eq!(restored.emote_lookup_channel, None);
        assert_eq!(restored.emote_lookup_source_id, None);
        assert_eq!(restored.reply_parent_user_id, None);
    }

    #[test]
    fn history_v1_rejects_trailing_payload_bytes() {
        let encoded =
            bincode::encode_to_vec(&[stored_message_v1_fixture()], bincode::config::standard())
                .expect("v1 history fixture should encode");
        let mut payload = encoded;
        payload.push(0);

        let error = decode_stored_messages(&payload)
            .map(|messages| messages.len())
            .expect_err("v1 history must consume the complete payload");
        assert!(matches!(
            error,
            HistoryCacheReadError::InvalidSnapshot {
                stage: "V1 payload",
                ..
            }
        ));
    }

    #[test]
    fn history_v2_defaults_emote_lookup_to_owning_channel() {
        let stored = vec![stored_message_v2_fixture()];
        let encoded = bincode::encode_to_vec(&stored, bincode::config::standard())
            .expect("v2 history fixture should encode");
        let mut payload = HISTORY_CACHE_V2_MAGIC.to_vec();
        payload.extend(encoded);

        let restored = decode_stored_messages(&payload)
            .expect("v2 history should decode")
            .pop()
            .expect("one row")
            .into_chat_message("host");

        assert_eq!(restored.emote_lookup_channel, None);
    }

    #[test]
    fn history_v2_rejects_trailing_payload_bytes() {
        let encoded =
            bincode::encode_to_vec(&[stored_message_v2_fixture()], bincode::config::standard())
                .expect("v2 history fixture should encode");
        let mut payload = HISTORY_CACHE_V2_MAGIC.to_vec();
        payload.extend(encoded);
        payload.push(0);

        let error = decode_stored_messages(&payload)
            .map(|messages| messages.len())
            .expect_err("v2 history must consume the complete payload");
        assert!(matches!(
            error,
            HistoryCacheReadError::InvalidSnapshot {
                stage: "V2 payload",
                ..
            }
        ));
    }
}
