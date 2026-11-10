//! Benchmark chat replay ingestion.
//!
//! This is intentionally gated behind environment variables so normal app
//! launches keep using live IRC. Replay messages are inserted through the same
//! backend message buffer and observer path as local/optimistic messages.

use std::collections::BTreeMap;
use std::io::BufRead;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::Utc;
use serde::Deserialize;
use serde_json::json;
use smallvec::SmallVec;

use crate::backend::irc::tmi_adapter::format_badge;
use crate::backend::models::{ChatMessage, MessageFlags, ReplyInfo, SystemMessageType};
use crate::backend::slots::compute_structure_hash;
use crate::backend::slots::parser::{parse_emotes_tag, parse_to_slots};

const REPLAY_PATH_ENV: &str = "ECHO_CHAT_REPLAY";
const REPLAY_REPORT_ENV: &str = "ECHO_CHAT_REPLAY_REPORT";
const REPLAY_CHANNEL_ENV: &str = "ECHO_CHAT_REPLAY_CHANNEL";
const REPLAY_DELAY_MS_ENV: &str = "ECHO_CHAT_REPLAY_DELAY_MS";
const REPLAY_SPEEDUP_ENV: &str = "ECHO_CHAT_REPLAY_SPEEDUP";
const REPLAY_MAX_EVENTS_ENV: &str = "ECHO_CHAT_REPLAY_MAX_EVENTS";
const REPLAY_CLEAR_CHANNEL_ENV: &str = "ECHO_CHAT_REPLAY_CLEAR_CHANNEL";
const REPLAY_PRESERVE_TIMESTAMPS_ENV: &str = "ECHO_CHAT_REPLAY_PRESERVE_TIMESTAMPS";
const REPLAY_INSERT_CONTROL_ENV: &str = "ECHO_CHAT_REPLAY_INSERT_CONTROL_MESSAGES";

/// Returns whether this process is configured to inject a benchmark chat replay.
///
/// A replay shares the ordinary channel buffers with live ingestion, so local
/// history persistence must remain disabled for the entire process. Checking
/// configuration instead of replay-generated message IDs also protects live
/// rows received while a benchmark fixture is active.
pub fn suppresses_history_persistence() -> bool {
    std::env::var_os(REPLAY_PATH_ENV).is_some()
}

#[derive(Clone, Debug)]
struct ReplayConfig {
    path: PathBuf,
    report_path: Option<PathBuf>,
    channel_override: Option<String>,
    delay_ms: u64,
    speedup: Option<f64>,
    max_events: Option<usize>,
    clear_channel: bool,
    preserve_timestamps: bool,
    insert_control_messages: bool,
}

#[derive(Default)]
struct ReplayStats {
    total_events: usize,
    inserted_messages: usize,
    inserted_control_messages: usize,
    control_events: usize,
    parse_errors: usize,
    skipped_events: usize,
    first_fixture_timestamp_ms: Option<i64>,
    last_fixture_timestamp_ms: Option<i64>,
    channels: BTreeMap<String, usize>,
}

#[derive(Debug, Deserialize)]
struct ReplayEvent {
    sequence: Option<u64>,
    kind: String,
    timestamp_ms: Option<i64>,
    channel: Option<String>,
    login: Option<String>,
    display_name: Option<String>,
    message: Option<ReplayMessage>,
    tags: Option<ReplayTags>,
    detail: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ReplayMessage {
    id: Option<String>,
    text: String,
}

#[derive(Debug, Default, Deserialize)]
struct ReplayTags {
    badges: Option<String>,
    color: Option<String>,
    display_name: Option<String>,
    emotes: Option<String>,
    gifs: Option<String>,
    user_id: Option<String>,
    reply_parent_msg_id: Option<String>,
    reply_parent_user_login: Option<String>,
    reply_parent_display_name: Option<String>,
    reply_parent_user_id: Option<String>,
    reply_parent_msg_body: Option<String>,
}

impl ReplayTags {
    /// Reply metadata, when the fixture event carries a parent message id.
    fn reply(&self) -> Option<ReplyInfo> {
        let parent_msg_id = self.reply_parent_msg_id.as_deref()?;
        let login = self
            .reply_parent_user_login
            .as_deref()
            .unwrap_or("benchuser");
        Some(ReplyInfo {
            parent_msg_id: Arc::from(parent_msg_id),
            parent_user_login: Arc::from(login),
            parent_display_name: Arc::from(
                self.reply_parent_display_name.as_deref().unwrap_or(login),
            ),
            parent_user_id: self.reply_parent_user_id.as_deref().map(Arc::from),
            parent_msg_body: self.reply_parent_msg_body.as_deref().map(Arc::from),
        })
    }
}

/// Start benchmark chat replay if `ECHO_CHAT_REPLAY` is set.
///
/// Returns true only when a replay worker was configured and spawned.
pub fn start_chat_replay_from_env() -> bool {
    let Some(config) = ReplayConfig::from_env() else {
        return false;
    };

    let path = config.path.clone();
    match std::thread::Builder::new()
        .name("echo-chat-replay".to_string())
        .spawn(move || run_chat_replay(config))
    {
        Ok(_) => {
            crate::log_irc!(info, "Started Echo chat replay worker for {:?}", path);
            true
        }
        Err(error) => {
            tracing::error!("Failed to spawn Echo chat replay worker: {error}");
            false
        }
    }
}

impl ReplayConfig {
    fn from_env() -> Option<Self> {
        let path = std::env::var_os(REPLAY_PATH_ENV).map(PathBuf::from)?;
        Some(Self {
            path,
            report_path: std::env::var_os(REPLAY_REPORT_ENV).map(PathBuf::from),
            channel_override: non_empty_env(REPLAY_CHANNEL_ENV).map(normalize_channel),
            delay_ms: env_u64(REPLAY_DELAY_MS_ENV).unwrap_or(0),
            speedup: env_f64(REPLAY_SPEEDUP_ENV).filter(|value| value.is_finite() && *value > 0.0),
            max_events: env_usize(REPLAY_MAX_EVENTS_ENV),
            clear_channel: env_flag(REPLAY_CLEAR_CHANNEL_ENV),
            preserve_timestamps: env_flag(REPLAY_PRESERVE_TIMESTAMPS_ENV),
            insert_control_messages: env_flag(REPLAY_INSERT_CONTROL_ENV),
        })
    }
}

fn run_chat_replay(config: ReplayConfig) {
    if config.delay_ms > 0 {
        std::thread::sleep(Duration::from_millis(config.delay_ms));
    }

    let started_at = Utc::now();
    let started = Instant::now();
    let mut stats = ReplayStats::default();
    let mut cleared_channels = std::collections::BTreeSet::new();
    let base_timestamp_ms = Utc::now().timestamp_millis();
    let mut previous_fixture_ts: Option<i64> = None;

    let result = std::fs::File::open(&config.path).map(std::io::BufReader::new);
    let decision = match result {
        Ok(reader) => {
            for (line_idx, line_result) in reader.lines().enumerate() {
                if config
                    .max_events
                    .is_some_and(|max| stats.total_events >= max)
                {
                    break;
                }

                let line = match line_result {
                    Ok(line) => line,
                    Err(error) => {
                        stats.parse_errors += 1;
                        tracing::warn!(
                            path = ?config.path,
                            line = line_idx + 1,
                            "Failed to read Echo chat replay line: {error}"
                        );
                        continue;
                    }
                };
                if line.trim().is_empty() {
                    continue;
                }

                let event: ReplayEvent = match serde_json::from_str(&line) {
                    Ok(event) => event,
                    Err(error) => {
                        stats.parse_errors += 1;
                        tracing::warn!(
                            path = ?config.path,
                            line = line_idx + 1,
                            "Failed to parse Echo chat replay line: {error}"
                        );
                        continue;
                    }
                };

                stats.total_events += 1;
                let fixture_ts = event.timestamp_ms.unwrap_or(0);
                if stats.first_fixture_timestamp_ms.is_none() {
                    stats.first_fixture_timestamp_ms = Some(fixture_ts);
                }
                stats.last_fixture_timestamp_ms = Some(fixture_ts);

                if let (Some(previous), Some(speedup)) = (previous_fixture_ts, config.speedup) {
                    let delta_ms = fixture_ts.saturating_sub(previous);
                    if delta_ms > 0 {
                        let paced_ms = (delta_ms as f64 / speedup).round() as u64;
                        if paced_ms > 0 {
                            std::thread::sleep(Duration::from_millis(paced_ms));
                        }
                    }
                }
                previous_fixture_ts = Some(fixture_ts);

                if event.kind == "message" {
                    match build_replay_message(
                        &event,
                        &config,
                        base_timestamp_ms,
                        stats.first_fixture_timestamp_ms.unwrap_or(fixture_ts),
                    ) {
                        Some((channel, message)) => {
                            if config.clear_channel && cleared_channels.insert(channel.clone()) {
                                crate::backend::irc::clear_channel_messages(&channel);
                            }
                            crate::backend::irc::insert_local_message(&channel, message);
                            *stats.channels.entry(channel).or_default() += 1;
                            stats.inserted_messages += 1;
                        }
                        None => {
                            stats.skipped_events += 1;
                        }
                    }
                } else if event.kind == "irc" {
                    // `detail` holds a raw CLEARMSG/CLEARCHAT line.
                    match event.detail.as_deref() {
                        Some(raw) if crate::backend::irc::replay_raw_moderation_line(raw) => {
                            stats.control_events += 1;
                        }
                        _ => stats.skipped_events += 1,
                    }
                } else {
                    stats.control_events += 1;
                    if config.insert_control_messages {
                        let channel = event_channel(&event, &config);
                        let message = build_control_message(&event, &channel, base_timestamp_ms);
                        if config.clear_channel && cleared_channels.insert(channel.clone()) {
                            crate::backend::irc::clear_channel_messages(&channel);
                        }
                        crate::backend::irc::insert_local_message(&channel, message);
                        stats.inserted_control_messages += 1;
                    }
                }
            }

            "Echo chat replay injected into backend message buffer; not compared"
        }
        Err(error) => {
            tracing::error!(path = ?config.path, "Failed to open Echo chat replay: {error}");
            "Echo chat replay failed before injection"
        }
    };

    let completed_at = Utc::now();
    write_replay_report(
        &config,
        &stats,
        started_at.to_rfc3339(),
        completed_at.to_rfc3339(),
        started.elapsed(),
        decision,
    );

    crate::log_irc!(
        info,
        path = ?config.path,
        total_events = stats.total_events,
        inserted_messages = stats.inserted_messages,
        control_events = stats.control_events,
        parse_errors = stats.parse_errors,
        "Echo chat replay completed"
    );
}

fn build_replay_message(
    event: &ReplayEvent,
    config: &ReplayConfig,
    base_timestamp_ms: i64,
    first_fixture_timestamp_ms: i64,
) -> Option<(String, ChatMessage)> {
    let message = event.message.as_ref()?;
    let channel = event_channel(event, config);
    if channel.is_empty() || message.text.trim().is_empty() {
        return None;
    }

    let tags = event.tags.as_ref();
    let login = event
        .login
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or("benchuser")
        .to_ascii_lowercase();
    let display_name = event
        .display_name
        .as_deref()
        .or_else(|| tags.and_then(|tags| tags.display_name.as_deref()))
        .filter(|value| !value.trim().is_empty())
        .unwrap_or(login.as_str());
    let (clean_text, is_action) = normalize_replay_message_text(&message.text);
    let emotes_tag = tags.and_then(|tags| tags.emotes.as_deref()).unwrap_or("");
    let twitch_emotes = parse_emotes_tag(emotes_tag);
    let gifs_tag = tags
        .and_then(|tags| tags.gifs.as_deref())
        .filter(|_| !is_action);
    let slots = crate::backend::slots::parser::parse_to_slots_with_gifs(
        &clean_text,
        &channel,
        crate::backend::emotes::EmoteSender::new(None, &login),
        &twitch_emotes,
        gifs_tag,
        0,
    );
    let structure_hash = compute_structure_hash(&slots);
    let timestamp = if config.preserve_timestamps {
        event.timestamp_ms.unwrap_or(base_timestamp_ms)
    } else {
        base_timestamp_ms
            + event
                .timestamp_ms
                .unwrap_or(0)
                .saturating_sub(first_fixture_timestamp_ms)
    };

    let chat_msg = ChatMessage {
        id: message
            .id
            .clone()
            .unwrap_or_else(|| format!("echo-replay-{}", event.sequence.unwrap_or(0)))
            .into(),
        channel: Arc::from(channel.as_str()),
        emote_lookup_channel: None,
        emote_lookup_source_id: None,
        sender: Arc::from(login.as_str()),
        display_name: Arc::from(display_name),
        content: Arc::from(clean_text.as_str()),
        color: tags
            .and_then(|tags| tags.color.as_deref())
            .filter(|value| !value.trim().is_empty())
            .map(Arc::from),
        badges: parse_badges(tags.and_then(|tags| tags.badges.as_deref()).unwrap_or("")),
        timestamp,
        slots: slots.into(),
        structure_hash,
        reply: tags.and_then(ReplayTags::reply),
        flags: MessageFlags {
            is_action,
            ..MessageFlags::default()
        },
        twitch_gifs_tag: gifs_tag.map(Arc::from),
        twitch_emotes_tag: (!emotes_tag.is_empty()).then(|| Arc::from(emotes_tag)),
        user_id: tags
            .and_then(|tags| tags.user_id.as_deref())
            .filter(|value| !value.trim().is_empty())
            .map(Arc::from),
        reply_emote_offset: 0,
    };

    Some((channel, chat_msg))
}

fn build_control_message(
    event: &ReplayEvent,
    channel: &str,
    base_timestamp_ms: i64,
) -> ChatMessage {
    let detail = event.detail.as_deref().unwrap_or("control event");
    let text = format!("[replay:{}] {detail}", event.kind);
    let slots = parse_to_slots(
        &text,
        channel,
        crate::backend::emotes::EmoteSender::NONE,
        &rustc_hash::FxHashMap::default(),
    );
    let structure_hash = compute_structure_hash(&slots);

    ChatMessage {
        id: format!("echo-replay-control-{}", event.sequence.unwrap_or(0)).into(),
        channel: Arc::from(channel),
        emote_lookup_channel: None,
        emote_lookup_source_id: None,
        sender: Arc::from("echo"),
        display_name: Arc::from("Echo"),
        content: Arc::from(text.as_str()),
        color: None,
        badges: SmallVec::new(),
        timestamp: base_timestamp_ms + event.timestamp_ms.unwrap_or(0),
        slots: slots.into(),
        structure_hash,
        reply: None,
        flags: MessageFlags {
            is_system: true,
            system_type: Some(SystemMessageType::ConnectionStatus {
                status: event.kind.clone(),
            }),
            ..MessageFlags::default()
        },
        twitch_gifs_tag: None,
        twitch_emotes_tag: None,
        user_id: None,
        reply_emote_offset: 0,
    }
}

fn event_channel(event: &ReplayEvent, config: &ReplayConfig) -> String {
    config
        .channel_override
        .clone()
        .unwrap_or_else(|| normalize_channel(event.channel.as_deref().unwrap_or("echo-bench")))
}

fn parse_badges(raw: &str) -> SmallVec<[Arc<str>; 4]> {
    raw.split(',')
        .filter_map(|badge| {
            let badge = badge.trim();
            if badge.is_empty() {
                return None;
            }
            let (name, version) = badge
                .split_once('/')
                .or_else(|| badge.split_once(':'))
                .unwrap_or((badge, "1"));
            Some(format_badge(name, version))
        })
        .collect()
}

fn normalize_replay_message_text(raw: &str) -> (String, bool) {
    if raw.starts_with("\x01ACTION ") && raw.ends_with('\x01') && raw.len() >= 9 {
        (raw[8..raw.len() - 1].to_string(), true)
    } else if let Some(stripped) = raw.strip_prefix("/me ") {
        (stripped.to_string(), true)
    } else {
        (raw.to_string(), false)
    }
}

fn normalize_channel(channel: impl AsRef<str>) -> String {
    channel
        .as_ref()
        .trim()
        .trim_start_matches('#')
        .to_ascii_lowercase()
}

fn non_empty_env(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn env_flag(name: &str) -> bool {
    std::env::var(name)
        .ok()
        .map(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(false)
}

fn env_u64(name: &str) -> Option<u64> {
    std::env::var(name)
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok())
}

fn env_usize(name: &str) -> Option<usize> {
    std::env::var(name)
        .ok()
        .and_then(|value| value.trim().parse::<usize>().ok())
}

fn env_f64(name: &str) -> Option<f64> {
    std::env::var(name)
        .ok()
        .and_then(|value| value.trim().parse::<f64>().ok())
}

fn write_replay_report(
    config: &ReplayConfig,
    stats: &ReplayStats,
    started_at_utc: String,
    completed_at_utc: String,
    elapsed: Duration,
    decision: &str,
) {
    let Some(path) = &config.report_path else {
        return;
    };

    if let Some(parent) = path.parent()
        && let Err(error) = std::fs::create_dir_all(parent)
    {
        tracing::warn!(path = ?parent, "Failed to create Echo chat replay report directory: {error}");
        return;
    }

    let report = json!({
        "schema_version": 1,
        "source": "echo-chat-replay",
        "replay_path": config.path.display().to_string(),
        "started_at_utc": started_at_utc,
        "completed_at_utc": completed_at_utc,
        "duration_ms": elapsed.as_millis(),
        "channel_override": config.channel_override.as_deref(),
        "delay_ms": config.delay_ms,
        "speedup": config.speedup,
        "max_events": config.max_events,
        "clear_channel": config.clear_channel,
        "preserve_timestamps": config.preserve_timestamps,
        "insert_control_messages": config.insert_control_messages,
        "total_events": stats.total_events,
        "inserted_messages": stats.inserted_messages,
        "inserted_control_messages": stats.inserted_control_messages,
        "control_events": stats.control_events,
        "parse_errors": stats.parse_errors,
        "skipped_events": stats.skipped_events,
        "first_fixture_timestamp_ms": stats.first_fixture_timestamp_ms,
        "last_fixture_timestamp_ms": stats.last_fixture_timestamp_ms,
        "channels": &stats.channels,
        "replay_injected": stats.inserted_messages > 0,
        "decision": decision,
    });

    match serde_json::to_vec_pretty(&report) {
        Ok(bytes) => {
            if let Err(error) = std::fs::write(path, bytes) {
                tracing::warn!(path = ?path, "Failed to write Echo chat replay report: {error}");
            }
        }
        Err(error) => {
            tracing::warn!("Failed to serialize Echo chat replay report: {error}");
        }
    }
}
