//! Readable per-day text chat logs: the writer thread, search, redaction of deleted
//! messages, and text exports.

use super::*;

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

pub(super) const HISTORY_EXPORTS_DIR_NAME: &str = "exports";
pub(super) const READABLE_LOG_HEADER_PREFIX: &str = "# Echo readable chat log for #";
pub(super) const HISTORY_EXPORT_HEADER_PREFIX: &str = "# Echo history export for #";

/// Get the history cache directory path.
pub fn history_dir_path() -> Option<PathBuf> {
    crate::storage_roots::config_root().map(|config_dir| config_dir.join("history"))
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
    crate::backend::settings::get_log_directory()
        .and_then(|path| {
            let trimmed = path.trim();
            (!trimmed.is_empty()).then(|| PathBuf::from(trimmed))
        })
        .or_else(|| crate::storage_roots::config_root().map(|config_dir| config_dir.join("logs")))
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

pub(super) const READABLE_LOG_FLUSH_INTERVAL_MS: u64 = 250;
pub(super) const READABLE_LOG_FLUSH_MAX_BUFFERED: usize = 256;
/// Jobs beyond this are dropped and counted rather than growing memory behind slow disk I/O.
pub(super) const READABLE_LOG_QUEUE_CAPACITY: usize = 10_000;
/// Recently written lines per channel whose file offsets are kept for in-place redaction.
pub(super) const READABLE_LOG_RECENT_LINES_PER_CHANNEL: usize = 2_048;
/// Tail of a daily log searched for a deleted row the recent index no longer holds.
pub(super) const READABLE_LOG_REDACT_SCAN_BYTES: u64 = 4 * 1024 * 1024;
pub(super) static READABLE_LOG_DROPPED_JOBS: AtomicU64 = AtomicU64::new(0);

/// Where a recently written, id-tagged line sits in its daily log file.
pub(super) struct ReadableLogRecentLine {
    pub(super) message_id: Box<str>,
    pub(super) path: Arc<Path>,
    pub(super) offset: u64,
    pub(super) len: usize,
}

/// A channel's recently written line offsets, across its daily files.
#[derive(Default)]
pub(super) struct ReadableLogChannelFile {
    pub(super) recent: std::collections::VecDeque<ReadableLogRecentLine>,
}

pub(super) type ReadableLogRecentIndex = FxHashMap<String, ReadableLogChannelFile>;
pub(super) type ReadableLogPending = FxHashMap<(String, chrono::NaiveDate), Vec<String>>;

#[allow(clippy::large_enum_variant)]
pub(super) enum ReadableLogJob {
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

pub(super) const READABLE_LOG_INTAKE_CLOSED: usize = 1 << (usize::BITS - 1);
pub(super) const READABLE_LOG_INTAKE_COUNT_MASK: usize = READABLE_LOG_INTAKE_CLOSED - 1;
pub(super) static READABLE_LOG_INTAKE_STATE: AtomicUsize = AtomicUsize::new(0);

pub(super) struct ReadableLogIntakeGuard;

impl Drop for ReadableLogIntakeGuard {
    fn drop(&mut self) {
        READABLE_LOG_INTAKE_STATE.fetch_sub(1, Ordering::Release);
    }
}

pub(super) fn begin_readable_log_intake() -> Option<ReadableLogIntakeGuard> {
    let previous = READABLE_LOG_INTAKE_STATE.fetch_add(1, Ordering::AcqRel);
    debug_assert!(previous & READABLE_LOG_INTAKE_COUNT_MASK != READABLE_LOG_INTAKE_COUNT_MASK);
    if previous & READABLE_LOG_INTAKE_CLOSED != 0 {
        READABLE_LOG_INTAKE_STATE.fetch_sub(1, Ordering::Release);
        None
    } else {
        Some(ReadableLogIntakeGuard)
    }
}

pub(super) fn close_readable_log_intake(
    started: std::time::Instant,
    timeout: std::time::Duration,
) -> bool {
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
pub(super) static LOG_FLUSH_WARN_LAST: Mutex<Option<std::time::Instant>> = Mutex::new(None);
/// Once-ever flag for send-failure warning (writer thread dead).
pub(super) static LOG_SEND_WARN_EMITTED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

pub(super) static READABLE_LOG_TX: OnceLock<crossbeam_channel::Sender<ReadableLogJob>> =
    OnceLock::new();

pub(super) fn readable_log_tx() -> &'static crossbeam_channel::Sender<ReadableLogJob> {
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

pub(super) fn readable_log_writer_loop(rx: crossbeam_channel::Receiver<ReadableLogJob>) {
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
pub(super) fn rate_limited_flush_warn(path: &std::path::Path, error: std::io::Error) {
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
pub(super) fn warn_log_send_failed() {
    use std::sync::atomic::Ordering;
    if !LOG_SEND_WARN_EMITTED.swap(true, Ordering::Relaxed) {
        crate::log_net!(warn, "chat logging disabled: writer thread unavailable");
    }
}

pub(super) fn flush_readable_log_buffers(
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
pub(super) fn readable_log_day_path(
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
pub(super) fn open_readable_log_day_file(
    path: &Path,
    channel_name: &str,
) -> Option<(std::fs::File, u64)> {
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

pub(super) fn note_readable_log_job_dropped() {
    let dropped = READABLE_LOG_DROPPED_JOBS.fetch_add(1, Ordering::Relaxed) + 1;
    if dropped.is_power_of_two() {
        crate::log_net!(
            warn,
            "Readable chat log writer is behind; dropped {} queued entries so far",
            dropped
        );
    }
}

pub(super) fn send_readable_log_job(job: ReadableLogJob) {
    match readable_log_tx().try_send(job) {
        Ok(()) => {}
        Err(crossbeam_channel::TrySendError::Full(_)) => note_readable_log_job_dropped(),
        Err(crossbeam_channel::TrySendError::Disconnected(_)) => warn_log_send_failed(),
    }
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
    if !crate::backend::settings::get_chat_logging_enabled() {
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
pub(super) fn record_readable_deleted_message_blocking(
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

    if !crate::backend::settings::get_log_deleted_messages() {
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
    pub(super) fn remove(&mut self, path: &Path) {
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

pub(super) fn read_dir_if_exists(dir: &Path) -> Result<Option<std::fs::ReadDir>, String> {
    match std::fs::read_dir(dir) {
        Ok(entries) => Ok(Some(entries)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(format!("Failed to read '{}': {error}", dir.display())),
    }
}

/// `<channel>-YYYY-MM-DD.<log|txt>` inside the `<channel>` folder.
pub(super) fn is_readable_day_log_file_name(channel: &str, file_name: &str) -> bool {
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
pub(super) fn is_history_export_file_name(file_name: &str) -> bool {
    let Some(stem) = file_name.strip_suffix(".txt") else {
        return false;
    };
    !stem.is_empty()
        && stem
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-')
}

pub(super) fn file_starts_with(path: &Path, prefix: &str) -> bool {
    let Ok(file) = std::fs::File::open(path) else {
        return false;
    };
    let mut head = Vec::with_capacity(prefix.len());
    file.take(prefix.len() as u64)
        .read_to_end(&mut head)
        .is_ok_and(|_| head == prefix.as_bytes())
}

pub(super) fn latest_history_file_path_in_dir(history_dir: &Path) -> Option<PathBuf> {
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

pub(super) fn latest_readable_log_file_path_in_dir(log_dir: &Path) -> Option<PathBuf> {
    readable_log_files_newest_first(log_dir)
        .ok()
        .and_then(|files| files.into_iter().next())
}

pub(super) fn read_readable_log_preview_file(
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

pub(super) fn search_readable_log_file(
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

pub(super) fn channel_name_from_history_path(path: &Path) -> Option<String> {
    path.file_name()
        .and_then(|name| name.to_str())
        .and_then(|name| name.strip_suffix(".bin.zst"))
        .map(ToString::to_string)
}

pub(super) fn sanitize_export_file_stem(value: &str) -> String {
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
pub(super) fn readable_log_files_newest_first(log_dir: &Path) -> Result<Vec<PathBuf>, String> {
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

pub(super) fn push_readable_log_file(
    files: &mut Vec<(SystemTime, PathBuf)>,
    entry: &std::fs::DirEntry,
) {
    let path = entry.path();
    let modified = entry
        .metadata()
        .and_then(|metadata| metadata.modified())
        .unwrap_or(SystemTime::UNIX_EPOCH);
    files.push((modified, path));
}

pub(super) fn has_readable_log_extension(path: &Path) -> bool {
    let Some(extension) = path.extension().and_then(|extension| extension.to_str()) else {
        return false;
    };

    matches!(extension.to_ascii_lowercase().as_str(), "log" | "txt")
}

/// Daily logs live at `<channel>/<channel>-YYYY-MM-DD.<ext>`; legacy single-file
/// logs are `<channel>.<ext>` directly in the log folder.
pub(super) fn channel_name_from_log_path(path: &Path) -> Option<String> {
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

pub(super) fn is_log_day_stamp(value: &str) -> bool {
    value.len() == 10
        && value.bytes().enumerate().all(|(index, byte)| match index {
            4 | 7 => byte == b'-',
            _ => byte.is_ascii_digit(),
        })
}

pub(super) fn readable_log_jump_target(
    channel: Option<&str>,
    line: &str,
) -> Option<ReadableLogJumpTarget> {
    let channel = channel?.trim().trim_start_matches('#');
    let channel = (!channel.is_empty()).then(|| channel.to_lowercase())?;
    let message_id = extract_log_message_id(line)?;

    Some(ReadableLogJumpTarget {
        channel,
        message_id,
    })
}

pub(super) fn extract_log_message_id(line: &str) -> Option<String> {
    const PREFIX: &str = "msg_id=";
    let start = line.find(PREFIX)? + PREFIX.len();
    let rest = &line[start..];
    let message_id = rest.split_whitespace().next()?.trim();
    (!message_id.is_empty()).then(|| message_id.to_string())
}

pub(super) fn format_history_export_timestamp(timestamp_ms: i64) -> String {
    Local
        .timestamp_millis_opt(timestamp_ms)
        .single()
        .map(|datetime| datetime.format("%Y-%m-%d %H:%M:%S").to_string())
        .unwrap_or_else(|| "unknown".to_string())
}

pub(super) fn single_line_history_text(text: &str) -> String {
    text.chars()
        .map(|ch| if ch == '\r' || ch == '\n' { ' ' } else { ch })
        .collect()
}

pub(super) fn readable_log_extension() -> String {
    let format = crate::backend::settings::get_log_format();
    let normalized = format.trim().trim_start_matches('.').to_ascii_lowercase();
    match normalized.as_str() {
        "log" => "log".to_string(),
        _ => "txt".to_string(),
    }
}

/// Local day of a message, or today when its timestamp is missing or more
/// than 36 hours from now (a bad clock or tag would scatter lines into
/// far-off day files).
pub(super) fn readable_log_day(timestamp_ms: i64) -> chrono::NaiveDate {
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

pub(super) fn format_readable_log_line(message: &ChatMessage) -> String {
    let timestamp = format_history_export_timestamp(message.timestamp);
    let timestamp = if crate::backend::settings::get_log_server_timestamps() {
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

pub(super) fn format_readable_delete_log_line(
    message_id: &str,
    sender_login: &str,
    original: Option<&ChatMessage>,
) -> String {
    let timestamp_ms = Utc::now().timestamp_millis();
    let timestamp = format_history_export_timestamp(timestamp_ms);
    let timestamp = if crate::backend::settings::get_log_server_timestamps() {
        format!("{timestamp} server_ts={timestamp_ms}")
    } else {
        timestamp
    };
    let message_id = log_field_value(message_id);
    let sender_login = log_field_value(sender_login);
    let mut line = format!("[{timestamp}] msg_id={message_id} [deleted] sender={sender_login}");

    if crate::backend::settings::get_log_deleted_messages()
        && let Some(original) = original
    {
        let limit = crate::backend::settings::get_deleted_log_char_limit().clamp(100, 5000);
        let content = truncate_log_text(&single_line_history_text(&original.content), limit);
        line.push_str(" original=");
        line.push_str(&quoted_log_field(&content));
    }

    line
}

/// Overwrite a recently appended row in place, keeping its byte length so no other
/// row moves and the file is never rewritten.
pub(super) fn redact_recent_readable_log_line(
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
pub(super) fn redact_readable_log_tail(path: &Path, message_id: &str) -> Result<bool, String> {
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
pub(super) fn find_redactable_readable_log_line(
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
pub(super) fn redactable_readable_log_line(line: &str, marker: &str) -> Option<String> {
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
pub(super) fn redacted_readable_log_line_in_place(line: &str, marker: &str) -> Option<String> {
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

pub(super) fn find_message_id_marker(line: &str, marker: &str) -> Option<usize> {
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

pub(super) fn log_field_value(value: impl AsRef<str>) -> String {
    value
        .as_ref()
        .trim()
        .chars()
        .map(|ch| if ch.is_whitespace() { '_' } else { ch })
        .collect()
}

pub(super) fn quoted_log_field(value: &str) -> String {
    let escaped = value.replace('\\', "\\\\").replace('"', "\\\"");
    format!("\"{escaped}\"")
}

pub(super) fn truncate_log_text(value: &str, max_chars: usize) -> String {
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

pub(super) fn get_readable_log_dir() -> Option<PathBuf> {
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
