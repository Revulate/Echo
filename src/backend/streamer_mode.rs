//! Streamer Mode - Privacy protection when streaming
//!
//! Provides streamer mode with:
//! - Auto-detection of streaming software (OBS, Streamlabs, XSplit)
//! - Manual override toggle
//! - Privacy controls (hide avatars, links, viewer counts, whispers, etc.)
//! - Mute mention sounds when streaming
//! - Suppress live notifications

use super::lock_utils::{MutexExt, RwLockExt};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex, RwLock};
use std::time::{Duration, Instant};

use sysinfo::{ProcessRefreshKind, RefreshKind, System};

/// Streamer mode status
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum StreamerModeStatus {
    /// Automatically enable when streaming software is detected
    #[default]
    Auto,
    /// Always enabled regardless of streaming software
    AlwaysOn,
    /// Always disabled
    Disabled,
}

/// Streamer mode settings
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct StreamerModeSettings {
    /// Streamer mode status (auto/always_on/disabled)
    #[serde(default)]
    pub enabled: StreamerModeStatus,

    /// Hide user avatars in user cards
    #[serde(default = "default_true")]
    pub hide_avatars: bool,

    /// Hide local user-card history and message details
    #[serde(default = "default_true")]
    pub hide_user_card_details: bool,

    /// Hide local account names in account switchers
    #[serde(default = "default_true")]
    pub hide_account_names: bool,

    /// Hide link thumbnails/previews
    #[serde(default = "default_true")]
    pub hide_link_thumbnails: bool,

    /// Hide visible link URLs in chat rows
    #[serde(default = "default_true")]
    pub hide_link_urls: bool,

    /// Hide viewer count and stream duration in headers
    #[serde(default = "default_true")]
    pub hide_viewer_count: bool,

    /// Hide moderation action details
    #[serde(default = "default_true")]
    pub hide_mod_actions: bool,

    /// Mute mention sounds when streaming
    #[serde(default = "default_true")]
    pub mute_mentions: bool,

    /// Suppress live notifications
    #[serde(default = "default_true")]
    pub suppress_notifications: bool,

    /// Hide whisper content (show placeholder)
    #[serde(default = "default_true")]
    pub hide_whisper_content: bool,

    /// Hide mention sidebar content (show placeholder)
    #[serde(default = "default_true")]
    pub hide_mention_content: bool,

    /// Hide AutoMod flagged terms
    #[serde(default = "default_true")]
    pub hide_automod_terms: bool,
}

fn default_true() -> bool {
    true
}

impl Default for StreamerModeSettings {
    fn default() -> Self {
        Self {
            enabled: StreamerModeStatus::Auto,
            hide_avatars: true,
            hide_user_card_details: true,
            hide_account_names: true,
            hide_link_thumbnails: true,
            hide_link_urls: true,
            hide_viewer_count: true,
            hide_mod_actions: true,
            mute_mentions: true,
            suppress_notifications: true,
            hide_whisper_content: true,
            hide_mention_content: true,
            hide_automod_terms: true,
        }
    }
}

/// Exact normalized process stems we consider to be streaming software.
const STREAMING_PROCESS_STEMS: &[&str] = &[
    "obs",
    "obs64",
    "obs32",
    "obsstudio",
    "obs studio",
    "obs-studio",
    "streamlabs obs",
    "streamlabs desktop",
    "streamlabsobs",
    "streamlabsdesktop",
    "xsplit.core",
    "xsplit.broadcaster",
    "xsplitcore",
    "xsplitbroadcaster",
    "gamecaster",
    "vmix",
    "vmix64",
    "prismlivestudio",
    "prism live studio",
    "prism",
    "twitchstudio",
    "twitch studio",
];

/// Conservative aliases for process names that commonly include branding words.
const STREAMING_PROCESS_ALIASES: &[&str] =
    &["streamlabs", "xsplit", "twitchstudio", "twitch studio"];

const DETECTION_INTERVAL: Duration = Duration::from_secs(2);
const MAX_ACTIVITY_LOG_WINDOW: u64 = 16 * 1024 * 1024;

#[derive(Default)]
struct ObsLogProbe {
    path: Option<PathBuf>,
    offset: u64,
    partial_line: String,
    streaming: bool,
    recording: bool,
}

#[derive(Default)]
struct DiscordLogProbe {
    offset: u64,
    partial_line: String,
    awaiting_connection: bool,
    active_connection: Option<String>,
}

#[derive(Default)]
struct CaptureProbeCache {
    obs: ObsLogProbe,
    discord: HashMap<PathBuf, DiscordLogProbe>,
}

#[derive(Default)]
struct DetectionResult {
    active: bool,
    source: Option<String>,
}

static CAPTURE_PROBE_CACHE: LazyLock<Mutex<CaptureProbeCache>> =
    LazyLock::new(|| Mutex::new(CaptureProbeCache::default()));

/// Detection state cache
struct DetectionCache {
    last_check: Option<Instant>,
    last_result: bool,
    last_source: Option<String>,
    refresh_in_flight: bool,
}

static DETECTION_CACHE: LazyLock<RwLock<DetectionCache>> = LazyLock::new(|| {
    RwLock::new(DetectionCache {
        last_check: None,
        last_result: false,
        last_source: None,
        refresh_in_flight: false,
    })
});

fn detection_cache_is_fresh(cache: &DetectionCache) -> bool {
    cache
        .last_check
        .is_some_and(|last_check| last_check.elapsed() < DETECTION_INTERVAL)
}

fn record_detection(detection: DetectionResult) -> bool {
    let is_running = detection.active;
    let result_changed = {
        let mut cache = DETECTION_CACHE.write_or_recover();
        let result_changed =
            cache.last_result != is_running || cache.last_source != detection.source;
        cache.last_check = Some(Instant::now());
        cache.last_result = is_running;
        cache.last_source = detection.source;
        cache.refresh_in_flight = false;
        result_changed
    };

    if result_changed {
        super::settings::broadcast_streamer_mode_changed();
    }

    is_running
}

/// Check if streaming software is running
/// Uses caching to avoid expensive process scans on every call
pub fn is_streaming_software_running() -> bool {
    {
        let mut cache = DETECTION_CACHE.write_or_recover();
        if detection_cache_is_fresh(&cache) || cache.refresh_in_flight {
            return cache.last_result;
        }
        cache.refresh_in_flight = true;
    }

    record_detection(detect_streaming_software_impl())
}

/// Schedule a single background refresh when the cached detection result is stale.
pub fn schedule_detection_refresh() {
    let should_schedule = {
        let mut cache = DETECTION_CACHE.write_or_recover();
        if detection_cache_is_fresh(&cache) || cache.refresh_in_flight {
            false
        } else {
            cache.refresh_in_flight = true;
            true
        }
    };

    if should_schedule {
        crate::backend::bridge::get_runtime().spawn_blocking(|| {
            let _ = record_detection(detect_streaming_software_impl());
        });
    }
}

fn detect_streaming_software_impl() -> DetectionResult {
    let refresh_kind = RefreshKind::nothing().with_processes(ProcessRefreshKind::nothing());
    let Ok(sys) = System::new_with_specifics(refresh_kind) else {
        return DetectionResult::default();
    };

    let mut obs_running = false;
    let mut discord_config_names = Vec::new();
    let mut generic_streaming_process = None;

    for process in sys.processes().values() {
        let process_name = process.name().to_string_lossy();
        let normalized = normalize_process_name(&process_name);

        if is_obs_process_name(&normalized) {
            obs_running = true;
        } else if let Some(config_name) = discord_config_name(&normalized) {
            if !discord_config_names.contains(&config_name) {
                discord_config_names.push(config_name);
            }
        } else if generic_streaming_process.is_none() && is_streaming_process_name(&process_name) {
            generic_streaming_process = Some(process_name.to_string());
        }
    }

    {
        let mut probes = CAPTURE_PROBE_CACHE.lock_or_recover();
        if obs_running {
            if let Some(source) = update_obs_activity(&mut probes.obs) {
                crate::log_ui!(debug, "Detected active OBS output: {}", source);
                return DetectionResult {
                    active: true,
                    source: Some(source),
                };
            }
        } else {
            probes.obs = ObsLogProbe::default();
        }

        if discord_config_names.is_empty() {
            probes.discord.clear();
        }
        for config_name in discord_config_names {
            let Some(path) = discord_renderer_log_path(config_name) else {
                continue;
            };
            let probe = probes.discord.entry(path.clone()).or_default();
            if update_discord_activity(&path, probe) {
                crate::log_ui!(debug, "Detected active Discord screen share");
                return DetectionResult {
                    active: true,
                    source: Some("Discord screen share is active".to_string()),
                };
            }
        }
    }

    if let Some(process_name) = generic_streaming_process {
        crate::log_ui!(debug, "Detected streaming software: {}", process_name);
        return DetectionResult {
            active: true,
            source: Some(format!("{process_name} detected")),
        };
    }

    DetectionResult::default()
}

fn is_obs_process_name(normalized: &str) -> bool {
    matches!(
        normalized,
        "obs" | "obs64" | "obs32" | "obsstudio" | "obs studio" | "obs-studio"
    )
}

fn discord_config_name(normalized: &str) -> Option<&'static str> {
    match normalized {
        "discord" => Some("discord"),
        "discordptb" => Some("discordptb"),
        "discordcanary" => Some("discordcanary"),
        "discorddevelopment" => Some("discorddevelopment"),
        _ => None,
    }
}

fn latest_obs_log_path() -> Option<PathBuf> {
    let log_dir = crate::storage_roots::external_config_dir()?
        .join("obs-studio")
        .join("logs");
    fs::read_dir(log_dir)
        .ok()?
        .filter_map(Result::ok)
        .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "txt"))
        .filter_map(|entry| {
            let modified = entry.metadata().ok()?.modified().ok()?;
            Some((modified, entry.path()))
        })
        .max_by_key(|(modified, _)| *modified)
        .map(|(_, path)| path)
}

fn discord_renderer_log_path(config_name: &str) -> Option<PathBuf> {
    let path = crate::storage_roots::external_config_dir()?
        .join(config_name)
        .join("logs")
        .join("renderer_js.log");
    path.is_file().then_some(path)
}

fn read_appended_log(
    path: &Path,
    offset: &mut u64,
    partial_line: &mut String,
) -> std::io::Result<(String, bool)> {
    let length = fs::metadata(path)?.len();
    let mut reset = length < *offset;
    if reset {
        *offset = 0;
        partial_line.clear();
    }

    if length.saturating_sub(*offset) > MAX_ACTIVITY_LOG_WINDOW {
        *offset = length - MAX_ACTIVITY_LOG_WINDOW;
        partial_line.clear();
        reset = true;
    }

    let starts_mid_file = *offset > 0 && reset;
    let mut file = File::open(path)?;
    file.seek(SeekFrom::Start(*offset))?;
    let mut bytes = Vec::with_capacity(length.saturating_sub(*offset).min(64 * 1024) as usize);
    file.read_to_end(&mut bytes)?;
    *offset = length;

    let mut text = std::mem::take(partial_line);
    text.push_str(&String::from_utf8_lossy(&bytes));
    if starts_mid_file {
        if let Some(newline) = text.find('\n') {
            text.drain(..=newline);
        } else {
            partial_line.push_str(&text);
            return Ok((String::new(), reset));
        }
    }

    if !text.ends_with('\n') {
        if let Some(newline) = text.rfind('\n') {
            partial_line.push_str(&text[newline + 1..]);
            text.truncate(newline + 1);
        } else {
            partial_line.push_str(&text);
            text.clear();
        }
    }

    Ok((text, reset))
}

fn update_obs_activity(probe: &mut ObsLogProbe) -> Option<String> {
    let path = latest_obs_log_path()?;
    if probe.path.as_ref() != Some(&path) {
        *probe = ObsLogProbe {
            path: Some(path.clone()),
            ..ObsLogProbe::default()
        };
    }

    let Ok((text, reset)) = read_appended_log(&path, &mut probe.offset, &mut probe.partial_line)
    else {
        probe.path = None;
        return None;
    };
    if reset {
        probe.streaming = false;
        probe.recording = false;
    }

    for line in text.lines() {
        if line.contains("==== Streaming Start") {
            probe.streaming = true;
        } else if line.contains("==== Streaming Stop") {
            probe.streaming = false;
        } else if line.contains("==== Recording Start") {
            probe.recording = true;
        } else if line.contains("==== Recording Stop") {
            probe.recording = false;
        }
    }

    match (probe.streaming, probe.recording) {
        (true, true) => Some("OBS is streaming and recording".to_string()),
        (true, false) => Some("OBS is streaming".to_string()),
        (false, true) => Some("OBS is recording".to_string()),
        (false, false) => None,
    }
}

fn discord_stream_connection_id(line: &str) -> Option<&str> {
    let rest = line.split_once("[RTCConnection(")?.1;
    rest.split_once(", stream)]").map(|(id, _)| id)
}

fn update_discord_activity(path: &Path, probe: &mut DiscordLogProbe) -> bool {
    let Ok((text, reset)) = read_appended_log(path, &mut probe.offset, &mut probe.partial_line)
    else {
        return false;
    };
    if reset {
        probe.awaiting_connection = false;
        probe.active_connection = None;
    }

    for line in text.lines() {
        if line.contains("[startStreamWithSource] Starting stream") {
            probe.awaiting_connection = true;
            continue;
        }

        let Some(connection_id) = discord_stream_connection_id(line) else {
            continue;
        };
        if probe.awaiting_connection && line.contains("Connecting to RTC server") {
            probe.active_connection = Some(connection_id.to_string());
            probe.awaiting_connection = false;
        } else if line.contains("Destroy RTCConnection")
            && probe.active_connection.as_deref() == Some(connection_id)
        {
            probe.active_connection = None;
            probe.awaiting_connection = false;
        }
    }

    probe.active_connection.is_some()
}

fn is_streaming_process_name(process_name: &str) -> bool {
    let normalized = normalize_process_name(process_name);
    if normalized.is_empty() {
        return false;
    }

    if STREAMING_PROCESS_STEMS.contains(&normalized.as_str()) {
        return true;
    }

    STREAMING_PROCESS_ALIASES
        .iter()
        .any(|alias| normalized.contains(alias))
}

fn normalize_process_name(process_name: &str) -> String {
    let trimmed = process_name.trim();
    if trimmed.is_empty() {
        return String::new();
    }

    let stem = Path::new(trimmed)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or(trimmed);

    stem.chars()
        .filter(|ch| ch.is_ascii_alphanumeric() || matches!(ch, ' ' | '-' | '.'))
        .flat_map(|ch| ch.to_lowercase())
        .collect()
}

/// Check if streamer mode is currently active
/// This combines the user's setting with auto-detection
pub fn is_streamer_mode_active() -> bool {
    let settings = get_streamer_mode_settings();

    is_active_for_settings(&settings)
}

/// Cached streamer-mode activity for UI rendering. A stale detection result is
/// refreshed in the background rather than scanning processes on the UI thread.
pub fn is_streamer_mode_active_cached() -> bool {
    let settings = get_streamer_mode_settings();
    match settings.enabled {
        StreamerModeStatus::AlwaysOn => true,
        StreamerModeStatus::Disabled => false,
        StreamerModeStatus::Auto => get_cached_detection_status().0,
    }
}

fn is_active_for_settings(settings: &StreamerModeSettings) -> bool {
    match settings.enabled {
        StreamerModeStatus::AlwaysOn => true,
        StreamerModeStatus::Disabled => false,
        StreamerModeStatus::Auto => is_streaming_software_running(),
    }
}

/// Check if viewer count and stream duration should be hidden.
pub fn should_hide_viewer_count() -> bool {
    let settings = get_streamer_mode_settings();
    settings.hide_viewer_count && is_active_for_settings(&settings)
}

/// Check if link preview thumbnails should be hidden.
pub fn should_hide_link_thumbnails() -> bool {
    let settings = get_streamer_mode_settings();
    settings.hide_link_thumbnails && is_active_for_settings(&settings)
}

/// Check if visible link URLs should be replaced with placeholders.
pub fn should_hide_link_urls() -> bool {
    let settings = get_streamer_mode_settings();
    settings.hide_link_urls && is_active_for_settings(&settings)
}

/// Check if profile/avatar images should be hidden.
pub fn should_hide_avatars() -> bool {
    let settings = get_streamer_mode_settings();
    settings.hide_avatars && is_active_for_settings(&settings)
}

/// Check if user-card local history/details should be hidden.
pub fn should_hide_user_card_details() -> bool {
    let settings = get_streamer_mode_settings();
    settings.hide_user_card_details && is_active_for_settings(&settings)
}

/// Check if local account names should be hidden.
pub fn should_hide_account_names() -> bool {
    let settings = get_streamer_mode_settings();
    settings.hide_account_names && is_active_for_settings(&settings)
}

/// Check if moderation action details should be hidden.
pub fn should_hide_mod_actions() -> bool {
    let settings = get_streamer_mode_settings();
    settings.hide_mod_actions && is_active_for_settings(&settings)
}

/// Check if AutoMod flagged terms and held-message details should be hidden.
pub fn should_hide_automod_terms() -> bool {
    let settings = get_streamer_mode_settings();
    settings.hide_automod_terms && is_active_for_settings(&settings)
}

/// Check if whisper message content should be hidden.
pub fn should_hide_whisper_content() -> bool {
    let settings = get_streamer_mode_settings();
    settings.hide_whisper_content && is_active_for_settings(&settings)
}

/// Check if mention sidebar content should be hidden.
pub fn should_hide_mention_content() -> bool {
    let settings = get_streamer_mode_settings();
    settings.hide_mention_content && is_active_for_settings(&settings)
}

/// Check if mention sounds should be muted.
pub fn should_mute_mentions() -> bool {
    let settings = get_streamer_mode_settings();
    settings.mute_mentions && is_active_for_settings(&settings)
}

/// Check if desktop notifications should be suppressed.
pub fn should_suppress_notifications() -> bool {
    let settings = get_streamer_mode_settings();
    settings.suppress_notifications && is_active_for_settings(&settings)
}

/// [`should_mute_mentions`] from cached detection, for UI-thread previews
/// that must not scan processes.
pub fn should_mute_mentions_cached() -> bool {
    get_streamer_mode_settings().mute_mentions && is_streamer_mode_active_cached()
}

/// [`should_suppress_notifications`] from cached detection, for UI-thread
/// previews that must not scan processes.
pub fn should_suppress_notifications_cached() -> bool {
    get_streamer_mode_settings().suppress_notifications && is_streamer_mode_active_cached()
}

/// Get streamer mode settings from global settings
pub fn get_streamer_mode_settings() -> StreamerModeSettings {
    super::settings::SETTINGS
        .lock_or_recover()
        .streamer_mode
        .clone()
        .unwrap_or_default()
}

/// Set streamer mode status
pub fn set_streamer_mode_status(status: StreamerModeStatus) {
    let changed = {
        let mut settings = super::settings::SETTINGS.lock_or_recover();
        let current_status = settings
            .streamer_mode
            .as_ref()
            .map(|sm| sm.enabled)
            .unwrap_or_default();
        if current_status == status {
            return;
        }

        settings
            .streamer_mode
            .get_or_insert_with(StreamerModeSettings::default)
            .enabled = status;
        true
    };
    if changed {
        super::settings::save_settings();
        super::settings::broadcast_streamer_mode_changed();
        crate::state::ui_invalidation::request(
            crate::state::ui_invalidation::UiInvalidation::TopBarState,
        );
    }
}

/// Update specific streamer mode settings
pub fn update_streamer_mode_settings<F>(update_fn: F)
where
    F: FnOnce(&mut StreamerModeSettings),
{
    let changed = {
        let mut settings = super::settings::SETTINGS.lock_or_recover();
        let streamer_mode = settings
            .streamer_mode
            .get_or_insert_with(StreamerModeSettings::default);
        let before = streamer_mode.clone();
        update_fn(streamer_mode);
        before != *streamer_mode
    };
    if changed {
        super::settings::save_settings();
        super::settings::broadcast_streamer_mode_changed();
        crate::state::ui_invalidation::request(
            crate::state::ui_invalidation::UiInvalidation::TopBarState,
        );
    }
}

/// Force a cache refresh for streaming software detection
/// Useful for manual testing or when the user wants to re-check
pub fn refresh_detection() {
    DETECTION_CACHE.write_or_recover().last_check = None;
    schedule_detection_refresh();
}

/// Get the latest cached detection status without scanning processes.
pub fn get_cached_detection_status() -> (bool, Option<String>) {
    schedule_detection_refresh();
    let cache = DETECTION_CACHE.read_or_recover();
    (cache.last_result, cache.last_source.clone())
}
