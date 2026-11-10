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

#[derive(Clone, Debug)]
pub struct ReadableLogPreview {
    pub path: PathBuf,
    pub byte_len: u64,
    pub lines: Vec<String>,
    pub truncated: bool,
}

#[derive(Clone, Debug)]
pub struct ReadableLogSearchMatch {
    pub scanned_line_index: usize,
    pub line: String,
    pub jump_target: Option<ReadableLogJumpTarget>,
}

#[derive(Clone, Debug)]
pub struct ReadableLogSearchResults {
    pub path: PathBuf,
    pub channel: String,
    pub byte_len: u64,
    pub scanned_byte_len: u64,
    pub scanned_line_count: usize,
    pub matches: Vec<ReadableLogSearchMatch>,
    pub match_limit_hit: bool,
    pub truncated: bool,
}

#[derive(Clone, Debug)]
pub struct ReadableLogJumpTarget {
    pub channel: String,
    pub message_id: String,
}

#[derive(Clone, Debug)]
pub struct ReadableLogGlobalSearchMatch {
    pub path: PathBuf,
    pub channel: String,
    pub scanned_line_index: usize,
    pub line: String,
    pub jump_target: Option<ReadableLogJumpTarget>,
}

#[derive(Clone, Debug)]
pub struct ReadableLogGlobalSearchResults {
    pub scanned_file_count: usize,
    pub total_file_count: usize,
    pub scanned_byte_len: u64,
    pub scanned_line_count: usize,
    pub matches: Vec<ReadableLogGlobalSearchMatch>,
    pub match_limit_hit: bool,
    pub file_limit_hit: bool,
    pub byte_limit_hit: bool,
    pub truncated: bool,
}

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

/// Frozen positional Postcard V4 DTO; keep its field order unchanged.
#[derive(Serialize, Deserialize, Clone)]
struct StoredMessageV4 {
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
}

impl From<StoredMessageV4> for StoredMessage {
    fn from(value: StoredMessageV4) -> Self {
        Self {
            id: value.id,
            sender: value.sender,
            display_name: value.display_name,
            content: value.content,
            emote_lookup_channel: value.emote_lookup_channel,
            emote_lookup_source_id: value.emote_lookup_source_id,
            color: value.color,
            badges: value.badges,
            timestamp: value.timestamp,
            user_id: value.user_id,
            twitch_emotes_tag: value.twitch_emotes_tag,
            reply_emote_offset: value.reply_emote_offset,
            is_action: value.is_action,
            reply_parent_msg_id: value.reply_parent_msg_id,
            reply_parent_user_login: value.reply_parent_user_login,
            reply_parent_display_name: value.reply_parent_display_name,
            reply_parent_msg_body: value.reply_parent_msg_body,
            reply_parent_user_id: value.reply_parent_user_id,
            twitch_gifs_tag: None,
        }
    }
}

/// Exact DTO written with `HISTORY_CACHE_V3_MAGIC`.
///
/// Keep this field order stable: Bincode's derived representation is positional,
/// so current Postcard DTO changes must not alter the V3 migration reader.
#[derive(Decode)]
#[cfg_attr(test, derive(Encode))]
struct StoredMessageV3 {
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
}

/// Exact DTO written with `HISTORY_CACHE_V2_MAGIC`.
#[derive(Decode)]
#[cfg_attr(test, derive(Encode))]
struct StoredMessageV2 {
    id: String,
    sender: String,
    display_name: String,
    content: String,
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
}

/// Exact DTO written before `HISTORY_CACHE_V2_MAGIC` was introduced.
///
/// Keep this field order stable: bincode's derived representation is positional,
/// so adding a field to `StoredMessage` without an explicit fallback would make
/// every existing local history file unreadable.
#[derive(Decode)]
#[cfg_attr(test, derive(Encode))]
struct StoredMessageV1 {
    id: String,
    sender: String,
    display_name: String,
    content: String,
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
}

impl From<StoredMessageV1> for StoredMessage {
    fn from(value: StoredMessageV1) -> Self {
        Self {
            twitch_gifs_tag: None,
            id: value.id,
            sender: value.sender,
            display_name: value.display_name,
            content: value.content,
            emote_lookup_channel: None,
            emote_lookup_source_id: None,
            color: value.color,
            badges: value.badges,
            timestamp: value.timestamp,
            user_id: value.user_id,
            twitch_emotes_tag: value.twitch_emotes_tag,
            reply_emote_offset: value.reply_emote_offset,
            is_action: value.is_action,
            reply_parent_msg_id: value.reply_parent_msg_id,
            reply_parent_user_login: value.reply_parent_user_login,
            reply_parent_display_name: value.reply_parent_display_name,
            reply_parent_msg_body: value.reply_parent_msg_body,
            reply_parent_user_id: None,
        }
    }
}

impl From<StoredMessageV2> for StoredMessage {
    fn from(value: StoredMessageV2) -> Self {
        Self {
            twitch_gifs_tag: None,
            id: value.id,
            sender: value.sender,
            display_name: value.display_name,
            content: value.content,
            emote_lookup_channel: None,
            emote_lookup_source_id: None,
            color: value.color,
            badges: value.badges,
            timestamp: value.timestamp,
            user_id: value.user_id,
            twitch_emotes_tag: value.twitch_emotes_tag,
            reply_emote_offset: value.reply_emote_offset,
            is_action: value.is_action,
            reply_parent_msg_id: value.reply_parent_msg_id,
            reply_parent_user_login: value.reply_parent_user_login,
            reply_parent_display_name: value.reply_parent_display_name,
            reply_parent_msg_body: value.reply_parent_msg_body,
            reply_parent_user_id: value.reply_parent_user_id,
        }
    }
}

impl From<StoredMessageV3> for StoredMessage {
    fn from(value: StoredMessageV3) -> Self {
        Self {
            twitch_gifs_tag: None,
            id: value.id,
            sender: value.sender,
            display_name: value.display_name,
            content: value.content,
            emote_lookup_channel: value.emote_lookup_channel,
            emote_lookup_source_id: value.emote_lookup_source_id,
            color: value.color,
            badges: value.badges,
            timestamp: value.timestamp,
            user_id: value.user_id,
            twitch_emotes_tag: value.twitch_emotes_tag,
            reply_emote_offset: value.reply_emote_offset,
            is_action: value.is_action,
            reply_parent_msg_id: value.reply_parent_msg_id,
            reply_parent_user_login: value.reply_parent_user_login,
            reply_parent_display_name: value.reply_parent_display_name,
            reply_parent_msg_body: value.reply_parent_msg_body,
            reply_parent_user_id: value.reply_parent_user_id,
        }
    }
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

fn is_legacy_synthetic_replay_message(message: &StoredMessage) -> bool {
    is_synthetic_replay_message_id(&message.id)
}

/// Moderator-removed rows keep their text in memory so a split can reveal them,
/// but the disk cache stores no flag for it: persisting one would bring the
/// removed message back as an ordinary row after a restart.
fn should_persist_history_message(message: &ChatMessage) -> bool {
    !message.flags.is_system
        && !message.flags.is_deleted
        && !crate::backend::whispers::is_whispers_channel(&message.channel)
        && !message.id.starts_with("optimistic-")
        && !is_synthetic_replay_message_id(message.id.as_ref())
}

const HISTORY_EXPORTS_DIR_NAME: &str = "exports";
const READABLE_LOG_HEADER_PREFIX: &str = "# Echo readable chat log for #";
const HISTORY_EXPORT_HEADER_PREFIX: &str = "# Echo history export for #";

/// Get the history cache directory path.
pub fn history_dir_path() -> Option<PathBuf> {
    crate::benchmark_profile::config_root().map(|config_dir| config_dir.join("history"))
}

/// Get the newest persisted channel history cache file, if any.
pub fn latest_history_file_path() -> Option<PathBuf> {
    let _io_guard = history_cache_io_guard();
    let history_dir = get_history_dir()?;
    latest_history_file_path_in_dir(&history_dir)
}

/// Get the folder holding readable text exports of the history cache.
pub fn history_exports_dir_path() -> Option<PathBuf> {
    history_dir_path().map(|history_dir| history_dir.join(HISTORY_EXPORTS_DIR_NAME))
}

/// Get the directory used for human-readable channel logs.
pub fn readable_log_dir_path() -> Option<PathBuf> {
    super::settings::get_log_directory()
        .and_then(|path| {
            let trimmed = path.trim();
            (!trimmed.is_empty()).then(|| PathBuf::from(trimmed))
        })
        .or_else(|| {
            crate::benchmark_profile::config_root().map(|config_dir| config_dir.join("logs"))
        })
}

/// Get the newest human-readable channel log file, if any.
pub fn latest_readable_log_file_path() -> Option<PathBuf> {
    let _io_guard = history_cache_io_guard();
    let log_dir = readable_log_dir_path()?;
    latest_readable_log_file_path_in_dir(&log_dir)
}

/// Read a bounded tail preview of the newest human-readable channel log.
pub fn latest_readable_log_preview(
    max_lines: usize,
    max_bytes: u64,
) -> Result<ReadableLogPreview, String> {
    let max_lines = max_lines.max(1);
    let max_bytes = max_bytes.max(1024);
    let _io_guard = history_cache_io_guard();
    let log_dir = readable_log_dir_path().ok_or("Readable log folder unavailable")?;
    let path =
        latest_readable_log_file_path_in_dir(&log_dir).ok_or("No readable channel logs yet")?;
    read_readable_log_preview_file(&path, max_lines, max_bytes)
}

/// Search the newest human-readable channel log using a bounded recent tail scan.
pub fn search_latest_readable_log(
    query: &str,
    max_matches: usize,
    max_bytes: u64,
) -> Result<ReadableLogSearchResults, String> {
    let normalized_query = query.trim();
    if normalized_query.is_empty() {
        return Err("Enter search text to filter the newest readable log".to_string());
    }

    let max_matches = max_matches.max(1);
    let max_bytes = max_bytes.max(1024);
    let _io_guard = history_cache_io_guard();
    let log_dir = readable_log_dir_path().ok_or("Readable log folder unavailable")?;
    let path =
        latest_readable_log_file_path_in_dir(&log_dir).ok_or("No readable channel logs yet")?;
    search_readable_log_file(&path, normalized_query, max_matches, max_bytes)
}

/// Search recent tails across multiple readable logs without loading the full archive.
pub fn search_readable_logs(
    query: &str,
    max_matches: usize,
    max_total_bytes: u64,
    max_files: usize,
    max_bytes_per_file: u64,
) -> Result<ReadableLogGlobalSearchResults, String> {
    let normalized_query = query.trim();
    if normalized_query.is_empty() {
        return Err("Enter search text to scan readable logs".to_string());
    }

    let max_matches = max_matches.max(1);
    let max_total_bytes = max_total_bytes.max(1024);
    let max_files = max_files.max(1);
    let max_bytes_per_file = max_bytes_per_file.max(1024);

    let _io_guard = history_cache_io_guard();
    let log_dir = readable_log_dir_path().ok_or("Readable log folder unavailable")?;
    let files = readable_log_files_newest_first(&log_dir)?;
    if files.is_empty() {
        return Err("No readable channel logs yet".to_string());
    }

    let total_file_count = files.len();
    let mut remaining_bytes = max_total_bytes;
    let mut scanned_file_count = 0usize;
    let mut scanned_byte_len = 0u64;
    let mut scanned_line_count = 0usize;
    let mut matches = Vec::new();
    let mut match_limit_hit = false;
    let mut file_limit_hit = false;
    let mut byte_limit_hit = false;
    let mut truncated = false;

    for path in files.into_iter().take(max_files) {
        if remaining_bytes < 1024 {
            byte_limit_hit = true;
            truncated = true;
            break;
        }

        let file_budget = remaining_bytes.min(max_bytes_per_file);
        let file_results = search_readable_log_file(
            &path,
            normalized_query,
            max_matches.saturating_sub(matches.len()).max(1),
            file_budget,
        )?;

        scanned_file_count += 1;
        scanned_byte_len += file_results.scanned_byte_len;
        scanned_line_count += file_results.scanned_line_count;
        remaining_bytes = remaining_bytes.saturating_sub(file_results.scanned_byte_len);
        truncated |=
            file_results.truncated || file_results.scanned_byte_len < file_results.byte_len;

        for entry in file_results.matches {
            matches.push(ReadableLogGlobalSearchMatch {
                path: file_results.path.clone(),
                channel: file_results.channel.clone(),
                scanned_line_index: entry.scanned_line_index,
                line: entry.line,
                jump_target: entry.jump_target,
            });
            if matches.len() >= max_matches {
                match_limit_hit = true;
                break;
            }
        }

        if match_limit_hit || file_results.match_limit_hit {
            match_limit_hit = true;
            break;
        }
    }

    if !match_limit_hit && total_file_count > scanned_file_count {
        file_limit_hit = scanned_file_count >= max_files;
        truncated |= file_limit_hit;
    }

    Ok(ReadableLogGlobalSearchResults {
        scanned_file_count,
        total_file_count,
        scanned_byte_len,
        scanned_line_count,
        matches,
        match_limit_hit,
        file_limit_hit,
        byte_limit_hit,
        truncated,
    })
}

const READABLE_LOG_FLUSH_INTERVAL_MS: u64 = 250;
const READABLE_LOG_FLUSH_MAX_BUFFERED: usize = 256;
/// Jobs beyond this are dropped and counted rather than growing memory behind slow disk I/O.
const READABLE_LOG_QUEUE_CAPACITY: usize = 10_000;
/// Recently written lines per channel whose file offsets are kept for in-place redaction.
const READABLE_LOG_RECENT_LINES_PER_CHANNEL: usize = 2_048;
/// Tail of a daily log searched for a deleted row the recent index no longer holds.
const READABLE_LOG_REDACT_SCAN_BYTES: u64 = 4 * 1024 * 1024;
static READABLE_LOG_DROPPED_JOBS: AtomicU64 = AtomicU64::new(0);

/// Where a recently written, id-tagged line sits in its daily log file.
struct ReadableLogRecentLine {
    message_id: Box<str>,
    path: Arc<Path>,
    offset: u64,
    len: usize,
}

/// A channel's recently written line offsets, across its daily files.
#[derive(Default)]
struct ReadableLogChannelFile {
    recent: std::collections::VecDeque<ReadableLogRecentLine>,
}

type ReadableLogRecentIndex = FxHashMap<String, ReadableLogChannelFile>;
type ReadableLogPending = FxHashMap<(String, chrono::NaiveDate), Vec<String>>;

#[allow(clippy::large_enum_variant)]
enum ReadableLogJob {
    Append {
        /// Sanitized file stem for the channel.
        channel: String,
        /// Local day of the message timestamp, which picks the daily file.
        day: chrono::NaiveDate,
        line: String,
    },
    RecordDeleted {
        channel: String,
        message_id: String,
        sender_login: String,
        original: Option<ChatMessage>,
    },
    Shutdown(std::sync::mpsc::Sender<()>),
}

const READABLE_LOG_INTAKE_CLOSED: usize = 1 << (usize::BITS - 1);
const READABLE_LOG_INTAKE_COUNT_MASK: usize = READABLE_LOG_INTAKE_CLOSED - 1;
static READABLE_LOG_INTAKE_STATE: AtomicUsize = AtomicUsize::new(0);

struct ReadableLogIntakeGuard;

impl Drop for ReadableLogIntakeGuard {
    fn drop(&mut self) {
        READABLE_LOG_INTAKE_STATE.fetch_sub(1, Ordering::Release);
    }
}

fn begin_readable_log_intake() -> Option<ReadableLogIntakeGuard> {
    let previous = READABLE_LOG_INTAKE_STATE.fetch_add(1, Ordering::AcqRel);
    debug_assert!(previous & READABLE_LOG_INTAKE_COUNT_MASK != READABLE_LOG_INTAKE_COUNT_MASK);
    if previous & READABLE_LOG_INTAKE_CLOSED != 0 {
        READABLE_LOG_INTAKE_STATE.fetch_sub(1, Ordering::Release);
        None
    } else {
        Some(ReadableLogIntakeGuard)
    }
}

fn close_readable_log_intake(started: std::time::Instant, timeout: std::time::Duration) -> bool {
    READABLE_LOG_INTAKE_STATE.fetch_or(READABLE_LOG_INTAKE_CLOSED, Ordering::AcqRel);
    while READABLE_LOG_INTAKE_STATE.load(Ordering::Acquire) & READABLE_LOG_INTAKE_COUNT_MASK != 0 {
        if started.elapsed() >= timeout {
            crate::log_net!(warn, "Timed out closing readable chat log intake");
            return false;
        }
        std::thread::yield_now();
    }
    true
}

/// Rate-limit state for flush-failure warnings (one per ~5 minutes).
static LOG_FLUSH_WARN_LAST: Mutex<Option<std::time::Instant>> = Mutex::new(None);
/// Once-ever flag for send-failure warning (writer thread dead).
static LOG_SEND_WARN_EMITTED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

static READABLE_LOG_TX: OnceLock<crossbeam_channel::Sender<ReadableLogJob>> = OnceLock::new();

fn readable_log_tx() -> &'static crossbeam_channel::Sender<ReadableLogJob> {
    READABLE_LOG_TX.get_or_init(|| {
        let (tx, rx) = crossbeam_channel::bounded(READABLE_LOG_QUEUE_CAPACITY);
        if let Err(error) = std::thread::Builder::new()
            .name("readable-chat-log".into())
            .spawn(move || readable_log_writer_loop(rx))
        {
            crate::log_net!(warn, "Failed to spawn readable chat log writer: {}", error);
        }
        tx
    })
}

fn readable_log_writer_loop(rx: crossbeam_channel::Receiver<ReadableLogJob>) {
    let mut pending = ReadableLogPending::default();
    let mut recent = ReadableLogRecentIndex::default();
    let mut buffered = 0usize;
    loop {
        let job = if buffered == 0 {
            rx.recv()
                .map_err(|_| crossbeam_channel::RecvTimeoutError::Disconnected)
        } else {
            rx.recv_timeout(std::time::Duration::from_millis(
                READABLE_LOG_FLUSH_INTERVAL_MS,
            ))
        };
        match job {
            Ok(ReadableLogJob::Append { channel, day, line }) => {
                pending.entry((channel, day)).or_default().push(line);
                buffered += 1;
                if buffered >= READABLE_LOG_FLUSH_MAX_BUFFERED {
                    flush_readable_log_buffers(&mut pending, &mut recent);
                    buffered = 0;
                }
            }
            Ok(ReadableLogJob::RecordDeleted {
                channel,
                message_id,
                sender_login,
                original,
            }) => {
                flush_readable_log_buffers(&mut pending, &mut recent);
                buffered = 0;
                record_readable_deleted_message_blocking(
                    &channel,
                    &message_id,
                    &sender_login,
                    original.as_ref(),
                    &mut recent,
                );
            }
            Ok(ReadableLogJob::Shutdown(ack)) => {
                flush_readable_log_buffers(&mut pending, &mut recent);
                let _ = ack.send(());
                return;
            }
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => {
                if buffered > 0 {
                    flush_readable_log_buffers(&mut pending, &mut recent);
                    buffered = 0;
                }
            }
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => {
                flush_readable_log_buffers(&mut pending, &mut recent);
                return;
            }
        }
    }
}

/// Emit a rate-limited warn (at most once per 5 minutes) for header/flush I/O failures.
fn rate_limited_flush_warn(path: &std::path::Path, error: std::io::Error) {
    const COOLDOWN: std::time::Duration = std::time::Duration::from_secs(300);
    let mut guard = LOG_FLUSH_WARN_LAST.lock_or_recover();
    let now = std::time::Instant::now();
    if guard.is_none_or(|last| now.duration_since(last) >= COOLDOWN) {
        *guard = Some(now);
        drop(guard);
        crate::log_net!(
            warn,
            "Failed to write readable chat log '{}': {}",
            path.display(),
            error
        );
    }
}

/// Emit a once-ever warn that the writer thread is unavailable.
fn warn_log_send_failed() {
    use std::sync::atomic::Ordering;
    if !LOG_SEND_WARN_EMITTED.swap(true, Ordering::Relaxed) {
        crate::log_net!(warn, "chat logging disabled: writer thread unavailable");
    }
}

fn flush_readable_log_buffers(
    pending: &mut ReadableLogPending,
    recent: &mut ReadableLogRecentIndex,
) {
    if pending.is_empty() {
        return;
    }

    let _io_guard = history_cache_io_guard();
    let Some(log_dir) = get_readable_log_dir() else {
        pending.clear();
        return;
    };
    let extension = readable_log_extension();

    for ((channel_name, day), lines) in pending.drain() {
        if lines.is_empty() {
            continue;
        }
        let path = readable_log_day_path(&log_dir, &channel_name, &extension, day);
        let Some((file, mut offset)) = open_readable_log_day_file(&path, &channel_name) else {
            continue;
        };
        let shared_path: Arc<Path> = Arc::from(path.as_path());
        let channel_file = recent.entry(channel_name).or_default();

        let mut writer = std::io::BufWriter::new(file);
        for line in &lines {
            if let Err(error) = writeln!(writer, "{line}") {
                crate::log_net!(
                    warn,
                    "Failed to append readable chat log '{}': {}",
                    path.display(),
                    error
                );
                channel_file.recent.clear();
                break;
            }
            if let Some(message_id) = extract_log_message_id(line) {
                if channel_file.recent.len() >= READABLE_LOG_RECENT_LINES_PER_CHANNEL {
                    channel_file.recent.pop_front();
                }
                channel_file.recent.push_back(ReadableLogRecentLine {
                    message_id: message_id.into_boxed_str(),
                    path: Arc::clone(&shared_path),
                    offset,
                    len: line.len(),
                });
            }
            offset += line.len() as u64 + 1;
        }
        if let Err(error) = writer.flush() {
            channel_file.recent.clear();
            rate_limited_flush_warn(&path, error);
        }
    }
}

/// `<log dir>/<channel>/<channel>-YYYY-MM-DD.<ext>`: one file per channel per local
/// day.
fn readable_log_day_path(
    log_dir: &Path,
    channel_name: &str,
    extension: &str,
    day: chrono::NaiveDate,
) -> PathBuf {
    log_dir
        .join(channel_name)
        .join(format!("{channel_name}-{day}.{extension}"))
}

/// Open a daily log for appending, writing its header when new, and return the
/// byte offset where the next line starts.
fn open_readable_log_day_file(path: &Path, channel_name: &str) -> Option<(std::fs::File, u64)> {
    if let Some(parent) = path.parent()
        && let Err(error) = std::fs::create_dir_all(parent)
    {
        rate_limited_flush_warn(path, error);
        return None;
    }
    let mut file = match std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        Ok(file) => file,
        Err(error) => {
            crate::log_net!(
                warn,
                "Failed to open readable chat log '{}': {}",
                path.display(),
                error
            );
            return None;
        }
    };
    let mut offset = file.metadata().map(|metadata| metadata.len()).unwrap_or(0);
    if offset == 0 {
        let header = format!("{READABLE_LOG_HEADER_PREFIX}{channel_name}\n");
        if let Err(error) = file.write_all(header.as_bytes()) {
            rate_limited_flush_warn(path, error);
            return None;
        }
        offset = header.len() as u64;
    }
    Some((file, offset))
}

fn note_readable_log_job_dropped() {
    let dropped = READABLE_LOG_DROPPED_JOBS.fetch_add(1, Ordering::Relaxed) + 1;
    if dropped.is_power_of_two() {
        crate::log_net!(
            warn,
            "Readable chat log writer is behind; dropped {} queued entries so far",
            dropped
        );
    }
}

fn send_readable_log_job(job: ReadableLogJob) {
    match readable_log_tx().try_send(job) {
        Ok(()) => {}
        Err(crossbeam_channel::TrySendError::Full(_)) => note_readable_log_job_dropped(),
        Err(crossbeam_channel::TrySendError::Disconnected(_)) => warn_log_send_failed(),
    }
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

/// Record a Twitch CLEARMSG deletion in the readable channel log.
///
/// Routed through the write-behind writer thread so the redaction's
/// read-modify-write lands after any buffered appends for the channel.
pub fn record_readable_deleted_message(
    channel: &str,
    message_id: &str,
    sender_login: &str,
    original: Option<&ChatMessage>,
) {
    if !super::settings::get_chat_logging_enabled() {
        return;
    }

    let message_id = message_id.trim();
    if message_id.is_empty() {
        return;
    }
    let Some(_intake) = begin_readable_log_intake() else {
        return;
    };

    send_readable_log_job(ReadableLogJob::RecordDeleted {
        channel: channel.to_string(),
        message_id: message_id.to_string(),
        sender_login: sender_login.to_string(),
        original: original.cloned(),
    });
}

/// Stop accepting readable-log work, drain the writer, and wait at most `timeout` for completion.
///
/// Completion means the worker attempted all queued writes; individual I/O
/// failures retain their existing warning-only, best-effort behavior.
pub(crate) fn shutdown_readable_chat_logs(timeout: std::time::Duration) -> bool {
    let started = std::time::Instant::now();
    if !close_readable_log_intake(started, timeout) {
        return false;
    }
    let Some(tx) = READABLE_LOG_TX.get() else {
        return true;
    };
    let (ack_tx, ack_rx) = std::sync::mpsc::channel();
    if tx
        .send_timeout(
            ReadableLogJob::Shutdown(ack_tx),
            timeout.saturating_sub(started.elapsed()),
        )
        .is_err()
    {
        warn_log_send_failed();
        return false;
    }
    match ack_rx.recv_timeout(timeout.saturating_sub(started.elapsed())) {
        Ok(()) => true,
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
            crate::log_net!(
                warn,
                "Timed out draining readable chat logs during shutdown"
            );
            false
        }
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
            crate::log_net!(
                warn,
                "Readable chat log writer stopped before acknowledging shutdown"
            );
            false
        }
    }
}

/// Readable log lines written by current Echo builds include `msg_id=...`.
/// When deleted-message logging is disabled, the deleted row is overwritten in
/// place with a same-length redacted placeholder: found through the recent
/// offset index, or else by scanning the tail of the message's daily file and
/// the day before it.
fn record_readable_deleted_message_blocking(
    channel: &str,
    message_id: &str,
    sender_login: &str,
    original: Option<&ChatMessage>,
    recent: &mut ReadableLogRecentIndex,
) {
    let _io_guard = history_cache_io_guard();
    let Some(log_dir) = get_readable_log_dir() else {
        return;
    };

    let channel_name = sanitize_export_file_stem(channel.trim_start_matches('#'));
    let extension = readable_log_extension();
    let path = readable_log_day_path(
        &log_dir,
        &channel_name,
        &extension,
        Local::now().date_naive(),
    );

    if !super::settings::get_log_deleted_messages() {
        let redacted = redact_recent_readable_log_line(recent.get_mut(&channel_name), message_id)
            .and_then(|found| {
                if found {
                    return Ok(true);
                }
                let day = original.map_or_else(
                    || Local::now().date_naive(),
                    |message| readable_log_day(message.timestamp),
                );
                for day in [Some(day), day.pred_opt()].into_iter().flatten() {
                    let day_path = readable_log_day_path(&log_dir, &channel_name, &extension, day);
                    if redact_readable_log_tail(&day_path, message_id)? {
                        return Ok(true);
                    }
                }
                Ok(false)
            });
        match redacted {
            Ok(true) => {}
            Ok(false) => {
                crate::log_net!(
                    debug,
                    "Deleted message {} was not found in the readable log for #{}",
                    message_id,
                    channel_name
                );
            }
            Err(error) => {
                crate::log_net!(
                    warn,
                    "Failed to redact readable log row for deleted message {} in #{}: {}",
                    message_id,
                    channel_name,
                    error
                );
            }
        }
    }

    let Some((mut file, _)) = open_readable_log_day_file(&path, &channel_name) else {
        return;
    };

    let marker = format_readable_delete_log_line(message_id, sender_login, original);
    if let Err(error) = writeln!(file, "{marker}") {
        crate::log_net!(
            warn,
            "Failed to append readable delete log '{}': {}",
            path.display(),
            error
        );
    }
}

/// Export the newest persisted channel history cache file as readable text.
pub fn export_latest_history_text_file() -> Result<PathBuf, String> {
    let _io_guard = history_cache_io_guard();
    let history_dir = get_history_dir().ok_or("History folder unavailable")?;
    let source_path = latest_history_file_path_in_dir(&history_dir)
        .ok_or("No saved channel history files yet")?;
    let channel = channel_name_from_history_path(&source_path).unwrap_or_else(|| "history".into());
    let messages = read_stored_history_file(&source_path).map_err(|error| error.to_string())?;

    let exports_dir = history_dir.join(HISTORY_EXPORTS_DIR_NAME);
    std::fs::create_dir_all(&exports_dir)
        .map_err(|e| format!("Failed to create history export directory: {e}"))?;
    let export_path = exports_dir.join(format!("{}.txt", sanitize_export_file_stem(&channel)));

    let mut output = String::new();
    output.push_str(&format!("{HISTORY_EXPORT_HEADER_PREFIX}{channel}\n"));
    output.push_str(&format!("# Source: {}\n", source_path.display()));
    output.push_str(&format!("# Messages: {}\n\n", messages.len()));

    for message in messages {
        let timestamp = format_history_export_timestamp(message.timestamp);
        let display_name = if message.display_name.trim().is_empty() {
            message.sender.as_str()
        } else {
            message.display_name.as_str()
        };
        let content = single_line_history_text(&message.content);
        if message.is_action {
            output.push_str(&format!("[{timestamp}] * {display_name} {content}\n"));
        } else {
            output.push_str(&format!("[{timestamp}] <{display_name}> {content}\n"));
        }
    }

    std::fs::write(&export_path, output)
        .map_err(|e| format!("Failed to write history text export: {e}"))?;
    Ok(export_path)
}

/// Files removed and files that could not be removed by a clear action.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ClearedFiles {
    pub removed: usize,
    pub failed: usize,
}

impl ClearedFiles {
    fn remove(&mut self, path: &Path) {
        match std::fs::remove_file(path) {
            Ok(()) => self.removed += 1,
            Err(error) => {
                self.failed += 1;
                crate::log_persist!(warn, "Failed to remove '{}': {}", path.display(), error);
            }
        }
    }
}

/// Delete the readable channel logs Echo wrote. The log folder is user-chosen
/// and may hold other files, so only files that are both Echo-named and start
/// with Echo's log header are touched: daily logs
/// `<channel>/<channel>-YYYY-MM-DD.<ext>`, and legacy top-level `<channel>.<ext>`
/// files. A file that cannot be removed is counted and skipped.
pub fn clear_readable_logs() -> Result<ClearedFiles, String> {
    let _io_guard = history_cache_io_guard();
    let Some(log_dir) = readable_log_dir_path() else {
        return Ok(ClearedFiles::default());
    };
    let Some(entries) = read_dir_if_exists(&log_dir)? else {
        return Ok(ClearedFiles::default());
    };
    let mut cleared = ClearedFiles::default();

    for entry in entries.flatten() {
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        let path = entry.path();
        if file_type.is_dir() {
            let Some(channel) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            let Ok(channel_entries) = std::fs::read_dir(&path) else {
                continue;
            };
            let removed_before = cleared.removed;
            for channel_entry in channel_entries.flatten() {
                let day_path = channel_entry.path();
                let is_day_log = channel_entry
                    .file_type()
                    .is_ok_and(|file_type| file_type.is_file())
                    && channel_entry
                        .file_name()
                        .to_str()
                        .is_some_and(|name| is_readable_day_log_file_name(&channel, name))
                    && file_starts_with(&day_path, READABLE_LOG_HEADER_PREFIX);
                if is_day_log {
                    cleared.remove(&day_path);
                }
            }
            if cleared.removed > removed_before {
                // Only succeeds when the channel folder is now empty.
                let _ = std::fs::remove_dir(&path);
            }
        } else if file_type.is_file()
            && has_readable_log_extension(&path)
            && file_starts_with(&path, READABLE_LOG_HEADER_PREFIX)
        {
            cleared.remove(&path);
        }
    }

    Ok(cleared)
}

/// Delete the text exports Echo wrote to the history exports folder. A file
/// that cannot be removed is counted and skipped.
pub fn clear_history_text_exports() -> Result<ClearedFiles, String> {
    let _io_guard = history_cache_io_guard();
    let Some(exports_dir) = history_exports_dir_path() else {
        return Ok(ClearedFiles::default());
    };
    let Some(entries) = read_dir_if_exists(&exports_dir)? else {
        return Ok(ClearedFiles::default());
    };
    let mut cleared = ClearedFiles::default();

    for entry in entries.flatten() {
        let path = entry.path();
        let is_export = entry.file_type().is_ok_and(|file_type| file_type.is_file())
            && entry
                .file_name()
                .to_str()
                .is_some_and(is_history_export_file_name)
            && file_starts_with(&path, HISTORY_EXPORT_HEADER_PREFIX);
        if is_export {
            cleared.remove(&path);
        }
    }

    Ok(cleared)
}

fn read_dir_if_exists(dir: &Path) -> Result<Option<std::fs::ReadDir>, String> {
    match std::fs::read_dir(dir) {
        Ok(entries) => Ok(Some(entries)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(format!("Failed to read '{}': {error}", dir.display())),
    }
}

/// `<channel>-YYYY-MM-DD.<log|txt>` inside the `<channel>` folder.
fn is_readable_day_log_file_name(channel: &str, file_name: &str) -> bool {
    let Some(rest) = file_name
        .strip_prefix(channel)
        .and_then(|rest| rest.strip_prefix('-'))
    else {
        return false;
    };
    let Some((day, extension)) = rest.rsplit_once('.') else {
        return false;
    };
    matches!(extension.to_ascii_lowercase().as_str(), "log" | "txt")
        && day.len() == 10
        && chrono::NaiveDate::parse_from_str(day, "%Y-%m-%d").is_ok()
}

/// `<stem>.txt` where the stem is what `sanitize_export_file_stem` produces.
fn is_history_export_file_name(file_name: &str) -> bool {
    let Some(stem) = file_name.strip_suffix(".txt") else {
        return false;
    };
    !stem.is_empty()
        && stem
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-')
}

fn file_starts_with(path: &Path, prefix: &str) -> bool {
    let Ok(file) = std::fs::File::open(path) else {
        return false;
    };
    let mut head = Vec::with_capacity(prefix.len());
    file.take(prefix.len() as u64)
        .read_to_end(&mut head)
        .is_ok_and(|_| head == prefix.as_bytes())
}

fn latest_history_file_path_in_dir(history_dir: &Path) -> Option<PathBuf> {
    let entries = std::fs::read_dir(history_dir).ok()?;
    let mut latest: Option<(SystemTime, PathBuf)> = None;

    for entry in entries.flatten() {
        let path = entry.path();
        let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if !file_name.ends_with(".bin.zst") {
            continue;
        }

        let modified = entry
            .metadata()
            .and_then(|metadata| metadata.modified())
            .unwrap_or(SystemTime::UNIX_EPOCH);
        let should_replace = match latest.as_ref() {
            Some((latest_modified, _)) => modified > *latest_modified,
            None => true,
        };
        if should_replace {
            latest = Some((modified, path));
        }
    }

    latest.map(|(_, path)| path)
}

fn latest_readable_log_file_path_in_dir(log_dir: &Path) -> Option<PathBuf> {
    readable_log_files_newest_first(log_dir)
        .ok()
        .and_then(|files| files.into_iter().next())
}

fn read_readable_log_preview_file(
    path: &Path,
    max_lines: usize,
    max_bytes: u64,
) -> Result<ReadableLogPreview, String> {
    let mut file = std::fs::File::open(path)
        .map_err(|e| format!("Failed to open readable channel log: {e}"))?;
    let byte_len = file
        .metadata()
        .map_err(|e| format!("Failed to inspect readable channel log: {e}"))?
        .len();
    let start = byte_len.saturating_sub(max_bytes);

    file.seek(SeekFrom::Start(start))
        .map_err(|e| format!("Failed to seek readable channel log: {e}"))?;

    let mut bytes = Vec::new();
    file.take(max_bytes)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("Failed to read readable channel log: {e}"))?;

    let decoded = String::from_utf8_lossy(&bytes);
    let mut text: &str = decoded.as_ref();
    let truncated_by_bytes = start > 0;
    if truncated_by_bytes && let Some(newline_idx) = text.find('\n') {
        text = &text[newline_idx + 1..];
    }

    let mut lines: Vec<String> = text.lines().map(ToString::to_string).collect();
    let truncated_by_lines = lines.len() > max_lines;
    if truncated_by_lines {
        lines = lines.split_off(lines.len() - max_lines);
    }

    Ok(ReadableLogPreview {
        path: path.to_path_buf(),
        byte_len,
        lines,
        truncated: truncated_by_bytes || truncated_by_lines,
    })
}

fn search_readable_log_file(
    path: &Path,
    query: &str,
    max_matches: usize,
    max_bytes: u64,
) -> Result<ReadableLogSearchResults, String> {
    let mut file = std::fs::File::open(path)
        .map_err(|e| format!("Failed to open readable channel log: {e}"))?;
    let byte_len = file
        .metadata()
        .map_err(|e| format!("Failed to inspect readable channel log: {e}"))?
        .len();
    let start = byte_len.saturating_sub(max_bytes);

    file.seek(SeekFrom::Start(start))
        .map_err(|e| format!("Failed to seek readable channel log: {e}"))?;

    let mut bytes = Vec::new();
    file.take(max_bytes)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("Failed to read readable channel log: {e}"))?;

    let scanned_byte_len = bytes.len() as u64;
    let decoded = String::from_utf8_lossy(&bytes);
    let mut text: &str = decoded.as_ref();
    let truncated_by_bytes = start > 0;
    if truncated_by_bytes && let Some(newline_idx) = text.find('\n') {
        text = &text[newline_idx + 1..];
    }

    let query_lower = query.to_lowercase();
    let mut matches = Vec::new();
    let mut scanned_line_count = 0usize;
    let mut match_limit_hit = false;
    let channel = channel_name_from_log_path(path);

    for (scanned_line_index, line) in text.lines().enumerate() {
        scanned_line_count = scanned_line_index + 1;
        if line.to_lowercase().contains(&query_lower) {
            matches.push(ReadableLogSearchMatch {
                scanned_line_index,
                line: line.to_string(),
                jump_target: readable_log_jump_target(channel.as_deref(), line),
            });
            if matches.len() >= max_matches {
                match_limit_hit = true;
                break;
            }
        }
    }

    Ok(ReadableLogSearchResults {
        path: path.to_path_buf(),
        channel: channel.unwrap_or_else(|| "unknown".to_string()),
        byte_len,
        scanned_byte_len,
        scanned_line_count,
        matches,
        match_limit_hit,
        truncated: truncated_by_bytes,
    })
}

fn channel_name_from_history_path(path: &Path) -> Option<String> {
    path.file_name()
        .and_then(|name| name.to_str())
        .and_then(|name| name.strip_suffix(".bin.zst"))
        .map(ToString::to_string)
}

fn sanitize_export_file_stem(value: &str) -> String {
    let sanitized: String = value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' {
                ch
            } else {
                '_'
            }
        })
        .collect();
    if sanitized.is_empty() {
        "history".to_string()
    } else {
        sanitized
    }
}

/// Each channel's newest daily log plus any legacy single-file logs, newest
/// first. Daily files are chosen by their day stamp, so only one file per
/// channel folder is stat'ed however many days the folder holds.
fn readable_log_files_newest_first(log_dir: &Path) -> Result<Vec<PathBuf>, String> {
    let entries = std::fs::read_dir(log_dir)
        .map_err(|error| format!("Failed to read readable log folder: {error}"))?;
    let mut files = Vec::new();

    for entry in entries.flatten() {
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_dir() {
            let Ok(channel_entries) = std::fs::read_dir(entry.path()) else {
                continue;
            };
            let newest_day = channel_entries
                .flatten()
                .filter(|channel_entry| {
                    channel_entry
                        .file_type()
                        .is_ok_and(|file_type| file_type.is_file())
                        && has_readable_log_extension(&channel_entry.path())
                })
                .max_by_key(std::fs::DirEntry::file_name);
            if let Some(newest_day) = newest_day {
                push_readable_log_file(&mut files, &newest_day);
            }
        } else if file_type.is_file() && has_readable_log_extension(&entry.path()) {
            push_readable_log_file(&mut files, &entry);
        }
    }

    files.sort_by(|(left_modified, left_path), (right_modified, right_path)| {
        right_modified
            .cmp(left_modified)
            .then_with(|| left_path.cmp(right_path))
    });

    Ok(files.into_iter().map(|(_, path)| path).collect())
}

fn push_readable_log_file(files: &mut Vec<(SystemTime, PathBuf)>, entry: &std::fs::DirEntry) {
    let path = entry.path();
    let modified = entry
        .metadata()
        .and_then(|metadata| metadata.modified())
        .unwrap_or(SystemTime::UNIX_EPOCH);
    files.push((modified, path));
}

fn has_readable_log_extension(path: &Path) -> bool {
    let Some(extension) = path.extension().and_then(|extension| extension.to_str()) else {
        return false;
    };

    matches!(extension.to_ascii_lowercase().as_str(), "log" | "txt")
}

/// Daily logs live at `<channel>/<channel>-YYYY-MM-DD.<ext>`; legacy single-file
/// logs are `<channel>.<ext>` directly in the log folder.
fn channel_name_from_log_path(path: &Path) -> Option<String> {
    let stem = path.file_stem()?.to_str()?;
    let parent = path
        .parent()
        .and_then(|parent| parent.file_name())
        .and_then(|name| name.to_str());
    if let Some(channel) = parent
        && stem
            .strip_prefix(channel)
            .and_then(|rest| rest.strip_prefix('-'))
            .is_some_and(is_log_day_stamp)
    {
        return Some(channel.to_string());
    }
    Some(stem.to_string())
}

fn is_log_day_stamp(value: &str) -> bool {
    value.len() == 10
        && value.bytes().enumerate().all(|(index, byte)| match index {
            4 | 7 => byte == b'-',
            _ => byte.is_ascii_digit(),
        })
}

fn readable_log_jump_target(channel: Option<&str>, line: &str) -> Option<ReadableLogJumpTarget> {
    let channel = channel?.trim().trim_start_matches('#');
    let channel = (!channel.is_empty()).then(|| channel.to_lowercase())?;
    let message_id = extract_log_message_id(line)?;

    Some(ReadableLogJumpTarget {
        channel,
        message_id,
    })
}

fn extract_log_message_id(line: &str) -> Option<String> {
    const PREFIX: &str = "msg_id=";
    let start = line.find(PREFIX)? + PREFIX.len();
    let rest = &line[start..];
    let message_id = rest.split_whitespace().next()?.trim();
    (!message_id.is_empty()).then(|| message_id.to_string())
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

fn format_history_export_timestamp(timestamp_ms: i64) -> String {
    Local
        .timestamp_millis_opt(timestamp_ms)
        .single()
        .map(|datetime| datetime.format("%Y-%m-%d %H:%M:%S").to_string())
        .unwrap_or_else(|| "unknown".to_string())
}

fn single_line_history_text(text: &str) -> String {
    text.chars()
        .map(|ch| if ch == '\r' || ch == '\n' { ' ' } else { ch })
        .collect()
}

fn readable_log_extension() -> String {
    let format = super::settings::get_log_format();
    let normalized = format.trim().trim_start_matches('.').to_ascii_lowercase();
    match normalized.as_str() {
        "log" => "log".to_string(),
        _ => "txt".to_string(),
    }
}

/// Local day of a message, or today when its timestamp is missing or more
/// than 36 hours from now (a bad clock or tag would scatter lines into
/// far-off day files).
fn readable_log_day(timestamp_ms: i64) -> chrono::NaiveDate {
    const MAX_SKEW_MS: u64 = 36 * 60 * 60 * 1000;
    let now = Local::now();
    if timestamp_ms.abs_diff(now.timestamp_millis()) > MAX_SKEW_MS {
        return now.date_naive();
    }
    Local
        .timestamp_millis_opt(timestamp_ms)
        .single()
        .map_or_else(|| now.date_naive(), |time| time.date_naive())
}

fn format_readable_log_line(message: &ChatMessage) -> String {
    let timestamp = format_history_export_timestamp(message.timestamp);
    let timestamp = if super::settings::get_log_server_timestamps() {
        format!("{timestamp} server_ts={}", message.timestamp)
    } else {
        timestamp
    };
    let content = single_line_history_text(&message.content);
    let message_id = log_field_value(&message.id);

    if message.flags.is_system {
        format!("[{timestamp}] msg_id={message_id} [system] {content}")
    } else {
        let display_name = if message.display_name.trim().is_empty() {
            message.sender.as_ref()
        } else {
            message.display_name.as_ref()
        };
        if message.flags.is_action {
            format!("[{timestamp}] msg_id={message_id} * {display_name} {content}")
        } else {
            format!("[{timestamp}] msg_id={message_id} <{display_name}> {content}")
        }
    }
}

fn format_readable_delete_log_line(
    message_id: &str,
    sender_login: &str,
    original: Option<&ChatMessage>,
) -> String {
    let timestamp_ms = Utc::now().timestamp_millis();
    let timestamp = format_history_export_timestamp(timestamp_ms);
    let timestamp = if super::settings::get_log_server_timestamps() {
        format!("{timestamp} server_ts={timestamp_ms}")
    } else {
        timestamp
    };
    let message_id = log_field_value(message_id);
    let sender_login = log_field_value(sender_login);
    let mut line = format!("[{timestamp}] msg_id={message_id} [deleted] sender={sender_login}");

    if super::settings::get_log_deleted_messages()
        && let Some(original) = original
    {
        let limit = super::settings::get_deleted_log_char_limit().clamp(100, 5000);
        let content = truncate_log_text(&single_line_history_text(&original.content), limit);
        line.push_str(" original=");
        line.push_str(&quoted_log_field(&content));
    }

    line
}

/// Overwrite a recently appended row in place, keeping its byte length so no other
/// row moves and the file is never rewritten.
fn redact_recent_readable_log_line(
    channel_file: Option<&mut ReadableLogChannelFile>,
    message_id: &str,
) -> Result<bool, String> {
    let Some(channel_file) = channel_file else {
        return Ok(false);
    };
    let message_id = log_field_value(message_id);
    let Some(index) = channel_file
        .recent
        .iter()
        .rposition(|line| *line.message_id == *message_id)
    else {
        return Ok(false);
    };
    let Some(entry) = channel_file.recent.remove(index) else {
        return Ok(false);
    };

    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&*entry.path)
        .map_err(|error| format!("open failed: {error}"))?;
    file.seek(SeekFrom::Start(entry.offset))
        .map_err(|error| format!("seek failed: {error}"))?;
    let mut bytes = vec![0; entry.len];
    file.read_exact(&mut bytes)
        .map_err(|error| format!("read failed: {error}"))?;
    let Ok(line) = String::from_utf8(bytes) else {
        return Ok(false);
    };
    let marker = format!("msg_id={message_id}");
    let Some(redacted) = redactable_readable_log_line(&line, &marker) else {
        return Ok(false);
    };
    file.seek(SeekFrom::Start(entry.offset))
        .map_err(|error| format!("seek failed: {error}"))?;
    file.write_all(redacted.as_bytes())
        .map_err(|error| format!("write failed: {error}"))?;
    Ok(true)
}

/// Redact the newest row for `message_id` within the last
/// [`READABLE_LOG_REDACT_SCAN_BYTES`] of a daily log, in place.
fn redact_readable_log_tail(path: &Path, message_id: &str) -> Result<bool, String> {
    let mut file = match std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(format!("open failed: {error}")),
    };
    let len = file
        .metadata()
        .map_err(|error| format!("metadata failed: {error}"))?
        .len();
    let start = len.saturating_sub(READABLE_LOG_REDACT_SCAN_BYTES);
    file.seek(SeekFrom::Start(start))
        .map_err(|error| format!("seek failed: {error}"))?;
    let mut bytes = Vec::with_capacity((len - start) as usize);
    file.read_to_end(&mut bytes)
        .map_err(|error| format!("read failed: {error}"))?;
    let marker = format!("msg_id={}", log_field_value(message_id));
    let Some((line_offset, redacted)) =
        find_redactable_readable_log_line(&bytes, start > 0, &marker)
    else {
        return Ok(false);
    };
    file.seek(SeekFrom::Start(start + line_offset as u64))
        .map_err(|error| format!("seek failed: {error}"))?;
    file.write_all(redacted.as_bytes())
        .map_err(|error| format!("write failed: {error}"))?;
    Ok(true)
}

/// Newest complete line in `bytes` that is a redactable row for `marker`, with
/// its byte offset. A leading partial line is skipped when `starts_mid_file`.
fn find_redactable_readable_log_line(
    bytes: &[u8],
    starts_mid_file: bool,
    marker: &str,
) -> Option<(usize, String)> {
    let first = if starts_mid_file {
        bytes.iter().position(|byte| *byte == b'\n')? + 1
    } else {
        0
    };
    let mut line_end = bytes.len();
    while line_end > first {
        let line_start = bytes[first..line_end]
            .iter()
            .rposition(|byte| *byte == b'\n')
            .map_or(first, |index| first + index + 1);
        if let Ok(line) = std::str::from_utf8(&bytes[line_start..line_end])
            && let Some(redacted) = redactable_readable_log_line(line, marker)
        {
            return Some((line_start, redacted));
        }
        if line_start == first {
            break;
        }
        line_end = line_start - 1;
    }
    None
}

/// Same-length redaction of one whole logged chat row. Anything that is not a
/// single row starting with its timestamp, or is a deletion marker or already
/// redacted, is refused so a stale offset can never overwrite another row.
fn redactable_readable_log_line(line: &str, marker: &str) -> Option<String> {
    if !line.starts_with('[') || line.contains('\n') {
        return None;
    }
    let rest = &line[find_message_id_marker(line, marker)? + marker.len()..];
    if rest.starts_with(" [deleted]") || rest.starts_with(" [redacted") {
        return None;
    }
    redacted_readable_log_line_in_place(line, marker)
}

/// Replace everything after the `msg_id=` field with a placeholder padded to the
/// original byte length, or `None` when the row no longer carries that id.
fn redacted_readable_log_line_in_place(line: &str, marker: &str) -> Option<String> {
    let marker_end = find_message_id_marker(line, marker)? + marker.len();
    let available = line.len() - marker_end;
    let placeholder = [" [redacted deleted message]", " [redacted]"]
        .into_iter()
        .find(|placeholder| placeholder.len() <= available)
        .unwrap_or("");
    Some(format!(
        "{}{placeholder}{}",
        &line[..marker_end],
        " ".repeat(available - placeholder.len())
    ))
}

fn find_message_id_marker(line: &str, marker: &str) -> Option<usize> {
    let mut search_start = 0usize;
    while let Some(offset) = line[search_start..].find(marker) {
        let marker_start = search_start + offset;
        let marker_end = marker_start + marker.len();
        let before_ok =
            marker_start == 0 || line.as_bytes()[marker_start - 1].is_ascii_whitespace();
        let after_ok = marker_end == line.len()
            || line
                .as_bytes()
                .get(marker_end)
                .is_some_and(u8::is_ascii_whitespace);
        if before_ok && after_ok {
            return Some(marker_start);
        }
        search_start = marker_end;
    }
    None
}

fn log_field_value(value: impl AsRef<str>) -> String {
    value
        .as_ref()
        .trim()
        .chars()
        .map(|ch| if ch.is_whitespace() { '_' } else { ch })
        .collect()
}

fn quoted_log_field(value: &str) -> String {
    let escaped = value.replace('\\', "\\\\").replace('"', "\\\"");
    format!("\"{escaped}\"")
}

fn truncate_log_text(value: &str, max_chars: usize) -> String {
    let mut truncated = String::new();
    for (idx, ch) in value.chars().enumerate() {
        if idx >= max_chars {
            truncated.push_str("...");
            return truncated;
        }
        truncated.push(ch);
    }
    truncated
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

fn get_readable_log_dir() -> Option<PathBuf> {
    let log_dir = readable_log_dir_path()?;
    if !log_dir.exists()
        && let Err(error) = std::fs::create_dir_all(&log_dir)
    {
        crate::log_net!(
            warn,
            "Failed to create readable chat log dir '{}': {}",
            log_dir.display(),
            error
        );
        return None;
    }
    Some(log_dir)
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
            !is_legacy_protocol_status_message(message)
                && !is_legacy_synthetic_replay_message(message)
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
