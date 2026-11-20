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
use std::sync::atomic::{AtomicBool, Ordering};
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

/// How often Auto's detection task starts a scan.
const DETECTION_INTERVAL: Duration = Duration::from_secs(2);
/// How long a completed detection stays trusted without another completing.
/// Auto fails closed on events (startup, a switch into Auto, a system wake);
/// this only covers the task dying or a missed wake. The task completes a scan
/// every [`DETECTION_INTERVAL`], or as soon as a slow scan finishes.
const DETECTION_BACKSTOP: Duration = Duration::from_secs(60);
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
    last_result: bool,
    last_source: Option<String>,
    refresh_in_flight: bool,
    /// When the last trusted detection finished; `None` before the first one
    /// and after each [`reset_detection_for_auto`].
    completed_at: Option<Instant>,
    /// Bumped by every reset. A scan claimed under an older generation took
    /// its snapshot before the reset, so its result is discarded.
    generation: u64,
    /// Set when a gate answered "active" only because no trusted result
    /// existed while the cached one said "not streaming". The next recorded
    /// result then rebuilds layouts and broadcasts; an untrusted period no
    /// gate observed costs nothing.
    untrusted_observed: AtomicBool,
}

static DETECTION_CACHE: LazyLock<RwLock<DetectionCache>> = LazyLock::new(|| {
    RwLock::new(DetectionCache {
        last_result: false,
        last_source: None,
        refresh_in_flight: false,
        completed_at: None,
        generation: 0,
        untrusted_observed: AtomicBool::new(false),
    })
});

/// Whether the last completed detection can be trusted at `now`: one exists
/// since the last reset and it finished within [`DETECTION_BACKSTOP`].
fn detection_trusted(cache: &DetectionCache, now: Instant) -> bool {
    cache.completed_at.is_some_and(|completed_at| {
        now.saturating_duration_since(completed_at) <= DETECTION_BACKSTOP
    })
}

/// Whether Auto mode treats streaming software as running. Without a trusted
/// detection this is true, so a user in Auto who is streaming never sees
/// unredacted content before a scan confirms they are not. Callers hold at
/// least the read lock, so the observation is never lost to a concurrent
/// [`record_detection_into`].
fn auto_detection_active(cache: &DetectionCache, now: Instant) -> bool {
    if detection_trusted(cache, now) {
        return cache.last_result;
    }
    if !cache.last_result {
        cache.untrusted_observed.store(true, Ordering::Relaxed);
    }
    true
}

/// What recording a finished scan requires of the caller.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DetectionOutcome {
    /// The scan was claimed before a reset and was not recorded.
    Discarded,
    Recorded {
        /// A gate failed closed against the cached result, so layouts built
        /// meanwhile must be rebuilt whatever the last broadcast state was.
        forget_layout_state: bool,
        broadcast: bool,
    },
}

/// Store a scan claimed under `generation` that finished at `now`, releasing
/// the claim either way.
fn record_detection_into(
    cache: &mut DetectionCache,
    detection: DetectionResult,
    generation: u64,
    now: Instant,
) -> DetectionOutcome {
    cache.refresh_in_flight = false;
    if generation != cache.generation {
        return DetectionOutcome::Discarded;
    }
    let observed = std::mem::take(cache.untrusted_observed.get_mut());
    let changed = cache.last_result != detection.active || cache.last_source != detection.source;
    cache.last_result = detection.active;
    cache.last_source = detection.source;
    cache.completed_at = Some(now);
    DetectionOutcome::Recorded {
        forget_layout_state: observed,
        broadcast: observed || changed,
    }
}

fn apply_detection_outcome(outcome: DetectionOutcome) {
    let DetectionOutcome::Recorded {
        forget_layout_state,
        broadcast,
    } = outcome
    else {
        return;
    };
    if forget_layout_state {
        super::settings::forget_streamer_layout_state();
    }
    if broadcast {
        super::settings::broadcast_streamer_mode_changed();
    }
}

/// Drop the completed detection so Auto fails closed until a scan claimed
/// after this reset finishes. Called on startup-equivalent events: a switch
/// into Auto (scans stop outside Auto, so the cached result can be
/// arbitrarily old) and a system wake.
fn reset_detection_for_auto(cache: &mut DetectionCache) {
    cache.completed_at = None;
    cache.generation = cache.generation.wrapping_add(1);
}

fn auto_selected() -> bool {
    get_streamer_mode_settings().enabled == StreamerModeStatus::Auto
}

/// Reset `cache` when replacement settings select Auto; returns whether they
/// do. `None` is the default, which is Auto.
fn reset_detection_for_replacement(
    cache: &mut DetectionCache,
    replacement: Option<&StreamerModeSettings>,
) -> bool {
    let auto = replacement
        .map(|settings| settings.enabled)
        .unwrap_or_default()
        == StreamerModeStatus::Auto;
    if auto {
        reset_detection_for_auto(cache);
    }
    auto
}

/// A settings import or reset to defaults replaces every setting at once and
/// may select Auto from another mode. Call with the settings lock held, as the
/// replacement is installed, with the replacement's streamer settings; when
/// they select Auto, Auto fails closed as on any switch into it. If this
/// returns true, call [`request_detection_scan_after_replacement`] once the
/// lock is released.
pub(crate) fn reset_detection_for_replaced_settings(
    replacement: Option<&StreamerModeSettings>,
) -> bool {
    reset_detection_for_replacement(&mut DETECTION_CACHE.write_or_recover(), replacement)
}

/// Second half of [`reset_detection_for_replaced_settings`]; the caller must
/// not hold the settings lock.
pub(crate) fn request_detection_scan_after_replacement() {
    request_detection_scan();
}

/// True while Auto has no completed detection since startup, the last switch
/// into Auto or the last system wake, so its gates are failing closed.
pub fn detection_pending() -> bool {
    auto_selected() && DETECTION_CACHE.read_or_recover().completed_at.is_none()
}

/// Streaming may have started while the system slept, so Auto fails closed
/// until a scan started after the wake completes.
pub fn note_system_wake() {
    if auto_selected() {
        reset_detection_for_auto(&mut DETECTION_CACHE.write_or_recover());
        request_detection_scan();
    }
}

/// Claim the next scan unless one is running. Returns the generation the
/// scan's result belongs to.
fn claim_detection_refresh(cache: &mut DetectionCache) -> Option<u64> {
    if cache.refresh_in_flight {
        return None;
    }
    cache.refresh_in_flight = true;
    Some(cache.generation)
}

/// Clears the in-flight claim when the detection task unwinds before it
/// records a result, so later scans are not refused forever. It leaves
/// `completed_at` alone: a scan that panicked proves nothing.
struct DetectionClaimGuard<'a> {
    cache: &'a RwLock<DetectionCache>,
    armed: bool,
}

impl Drop for DetectionClaimGuard<'_> {
    fn drop(&mut self) {
        if self.armed {
            self.cache.write_or_recover().refresh_in_flight = false;
        }
    }
}

/// Run `detect` for a claimed refresh, releasing the claim if it panics.
fn run_claimed_detection(
    cache: &RwLock<DetectionCache>,
    detect: impl FnOnce() -> DetectionResult,
) -> DetectionResult {
    let mut guard = DetectionClaimGuard { cache, armed: true };
    let detection = detect();
    guard.armed = false;
    detection
}

/// Whether Auto's detection task is running on the backend runtime.
static DETECTION_TASK_RUNNING: AtomicBool = AtomicBool::new(false);
/// Asks the detection task for a scan now instead of at its next tick.
static DETECTION_SCAN_NOW: tokio::sync::Notify = tokio::sync::Notify::const_new();

/// Start Auto's detection task unless it is running. Does nothing outside
/// Auto. The caller must not hold the settings lock.
pub fn ensure_detection_task() {
    if auto_selected() {
        start_detection_task();
    }
}

/// Start the detection task for a caller that knows Auto is selected. The
/// common case is one atomic load.
fn start_detection_task() {
    if DETECTION_TASK_RUNNING.load(Ordering::Acquire)
        || DETECTION_TASK_RUNNING.swap(true, Ordering::AcqRel)
    {
        return;
    }
    DETECTION_SCAN_NOW.notify_one();
    crate::backend::bridge::get_runtime().spawn(async {
        {
            let _running = DetectionTaskRunning(&DETECTION_TASK_RUNNING);
            run_detection_loop(
                &DETECTION_CACHE,
                &DETECTION_SCAN_NOW,
                DETECTION_INTERVAL,
                auto_selected,
                detect_streaming_software_impl,
                apply_detection_outcome,
            )
            .await;
        }
        // Auto may have been selected again after the loop last checked.
        ensure_detection_task();
    });
}

/// Clears the task-running flag however the detection task ends: the loop
/// returning, a panic anywhere in its body (the outcome's broadcast, the
/// logging), or the future being dropped. The next gate read in Auto then
/// starts the task again instead of finding it marked running forever.
struct DetectionTaskRunning(&'static AtomicBool);

impl Drop for DetectionTaskRunning {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

/// Scan as soon as the task is free, starting it if needed. Auto must be
/// selected.
fn request_detection_scan() {
    start_detection_task();
    DETECTION_SCAN_NOW.notify_one();
}

/// Auto's detection cadence: a scan every `period` (or at once when
/// `scan_now` is notified) on the blocking pool, awaited so two never run
/// at once, independent of the UI thread. Returns once `still_auto` is false
/// at a tick. A panicking scan releases its claim and the loop carries on.
async fn run_detection_loop(
    cache: &'static RwLock<DetectionCache>,
    scan_now: &tokio::sync::Notify,
    period: Duration,
    still_auto: impl Fn() -> bool,
    detect: fn() -> DetectionResult,
    mut on_outcome: impl FnMut(DetectionOutcome),
) {
    let mut ticks = tokio::time::interval_at(tokio::time::Instant::now() + period, period);
    // A scan that overruns the period does not start the next one back to
    // back; the cadence resumes at the next whole period.
    ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    // Debug logging happens on change only: the first scan, a new result or
    // outcome, a discarded scan, the first failure of a run of failures. A
    // steady state would otherwise log every period.
    let mut last_logged: Option<(bool, Option<String>, DetectionOutcome)> = None;
    let mut failure_logged = false;
    loop {
        tokio::select! {
            _ = ticks.tick() => {}
            () = scan_now.notified() => ticks.reset(),
        }
        if !still_auto() {
            return;
        }
        let Some(generation) = claim_detection_refresh(&mut cache.write_or_recover()) else {
            continue;
        };
        let started = Instant::now();
        let scan = tokio::task::spawn_blocking(move || run_claimed_detection(cache, detect));
        let Ok(detection) = scan.await else {
            // It panicked or never ran; either way nothing holds the claim.
            cache.write_or_recover().refresh_in_flight = false;
            if !std::mem::replace(&mut failure_logged, true) {
                crate::log_ui!(warn, "Streamer-mode detection scan did not finish");
            }
            continue;
        };
        failure_logged = false;
        let duration_ms = started.elapsed().as_millis() as u64;
        let (active, source) = (detection.active, detection.source.clone());
        let outcome = record_detection_into(
            &mut cache.write_or_recover(),
            detection,
            generation,
            Instant::now(),
        );
        let logged = (active, source, outcome);
        if last_logged.as_ref() != Some(&logged) {
            crate::log_ui!(
                debug,
                duration_ms,
                active,
                outcome = ?outcome,
                "Streamer-mode detection result changed"
            );
            let previous_source = last_logged
                .as_ref()
                .and_then(|(_, source, _)| source.as_deref());
            if let Some(source) = logged.1.as_deref()
                && previous_source != Some(source)
            {
                crate::log_ui!(debug, "Detected streaming software: {}", source);
            }
            last_logged = Some(logged);
        }
        on_outcome(outcome);
    }
}

/// Auto mode's cached detection result. Never scans on the calling thread;
/// the detection task keeps it current.
fn cached_auto_detection_active() -> bool {
    start_detection_task();
    auto_detection_active(&DETECTION_CACHE.read_or_recover(), Instant::now())
}

fn detect_streaming_software_impl() -> DetectionResult {
    // Names only. `ProcessRefreshKind::nothing()` still lists every thread of
    // every process as an entry on Linux (`tasks` defaults to on), which costs
    // a `/proc/<pid>/task` listing and a read per thread; processes are listed
    // without it. A `System` is built per scan rather than kept: on Windows a
    // kept one holds an open handle to every process between scans, and on
    // Linux a refresh rereads each process's `stat` and `status` anyway.
    let refresh_kind =
        RefreshKind::nothing().with_processes(ProcessRefreshKind::nothing().without_tasks());
    let Ok(sys) = System::new_with_specifics(refresh_kind) else {
        return DetectionResult::default();
    };

    let mut obs_running = false;
    let mut discord_config_names = Vec::new();
    let mut generic_streaming_process = None;

    // Kernel threads (and any thread entry) carry thread names, not process
    // names, so they never identify streaming software.
    for process in sys
        .processes()
        .values()
        .filter(|process| process.thread_kind().is_none())
    {
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
                return DetectionResult {
                    active: true,
                    source: Some("Discord screen share is active".to_string()),
                };
            }
        }
    }

    if let Some(process_name) = generic_streaming_process {
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
/// This combines the user's setting with auto-detection. Auto reads the
/// cached detection result and refreshes a stale one in the background.
pub fn is_streamer_mode_active() -> bool {
    let settings = get_streamer_mode_settings();

    is_active_for_settings(&settings)
}

/// Same as [`is_streamer_mode_active`]; every gate now reads cached detection.
pub fn is_streamer_mode_active_cached() -> bool {
    is_streamer_mode_active()
}

fn is_active_for_settings(settings: &StreamerModeSettings) -> bool {
    match settings.enabled {
        StreamerModeStatus::AlwaysOn => true,
        StreamerModeStatus::Disabled => false,
        StreamerModeStatus::Auto => cached_auto_detection_active(),
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

/// Same as [`should_mute_mentions`]; every gate now reads cached detection.
pub fn should_mute_mentions_cached() -> bool {
    should_mute_mentions()
}

/// Same as [`should_suppress_notifications`]; every gate now reads cached
/// detection.
pub fn should_suppress_notifications_cached() -> bool {
    should_suppress_notifications()
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
        if status == StreamerModeStatus::Auto {
            reset_detection_for_auto(&mut DETECTION_CACHE.write_or_recover());
        }
        true
    };
    if changed {
        if status == StreamerModeStatus::Auto {
            request_detection_scan();
        }
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
    let (changed, entered_auto) = {
        let mut settings = super::settings::SETTINGS.lock_or_recover();
        let streamer_mode = settings
            .streamer_mode
            .get_or_insert_with(StreamerModeSettings::default);
        let before = streamer_mode.clone();
        update_fn(streamer_mode);
        let entered_auto = before.enabled != StreamerModeStatus::Auto
            && streamer_mode.enabled == StreamerModeStatus::Auto;
        if entered_auto {
            reset_detection_for_auto(&mut DETECTION_CACHE.write_or_recover());
        }
        (before != *streamer_mode, entered_auto)
    };
    if entered_auto {
        request_detection_scan();
    }
    if changed {
        super::settings::save_settings();
        super::settings::broadcast_streamer_mode_changed();
        crate::state::ui_invalidation::request(
            crate::state::ui_invalidation::UiInvalidation::TopBarState,
        );
    }
}

/// Ask Auto's detection task to scan now rather than at its next tick, for a
/// user who wants to re-check. Does nothing outside Auto.
pub fn refresh_detection() {
    if auto_selected() {
        request_detection_scan();
    }
}

/// Get the latest cached detection status without scanning processes.
pub fn get_cached_detection_status() -> (bool, Option<String>) {
    let cache = DETECTION_CACHE.read_or_recover();
    (cache.last_result, cache.last_source.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test;
    use std::sync::atomic::AtomicUsize;

    fn cache(completed: bool, last_result: bool) -> DetectionCache {
        DetectionCache {
            last_result,
            last_source: None,
            refresh_in_flight: false,
            completed_at: completed.then(Instant::now),
            generation: 0,
            untrusted_observed: AtomicBool::new(false),
        }
    }

    fn not_streaming() -> DetectionResult {
        DetectionResult::default()
    }

    /// Claim a scan and record `detection` as finishing at `now`.
    fn scan(
        cache: &mut DetectionCache,
        detection: DetectionResult,
        now: Instant,
    ) -> DetectionOutcome {
        let generation = claim_detection_refresh(cache).expect("no scan in flight");
        record_detection_into(cache, detection, generation, now)
    }

    fn recorded(forget_layout_state: bool, broadcast: bool) -> DetectionOutcome {
        DetectionOutcome::Recorded {
            forget_layout_state,
            broadcast,
        }
    }

    #[test]
    fn auto_counts_as_active_until_the_first_detection_completes() {
        let now = Instant::now();
        assert!(auto_detection_active(&cache(false, false), now));
        assert!(!auto_detection_active(&cache(true, false), now));
        assert!(auto_detection_active(&cache(true, true), now));
    }

    #[test]
    fn every_tick_completes_a_scan_once_per_detection_period() {
        // The task ticks every DETECTION_INTERVAL, a little late at times, and
        // each tick claims a scan; scans here take 40 ms or 900 ms. No tick is
        // refused because the previous result still looks fresh (that halved
        // the polling coordinator's rate to one scan every two periods).
        let mut cache = cache(false, false);
        let start = Instant::now();
        let mut completions = Vec::new();
        for tick in 0..10u32 {
            let lateness = Duration::from_millis(u64::from(tick % 3) * 15);
            let ticked_at = start + DETECTION_INTERVAL * tick + lateness;
            let generation = claim_detection_refresh(&mut cache).expect("every tick claims a scan");
            let scan_time = Duration::from_millis(if tick % 2 == 0 { 40 } else { 900 });
            let finished_at = ticked_at + scan_time;
            let outcome =
                record_detection_into(&mut cache, not_streaming(), generation, finished_at);
            assert_ne!(outcome, DetectionOutcome::Discarded);
            completions.push(finished_at);
        }
        for pair in completions.windows(2) {
            let gap = pair[1] - pair[0];
            assert!(
                gap < DETECTION_INTERVAL * 2,
                "a tick went without a scan: {gap:?}"
            );
        }
        let mean_gap = (completions[9] - completions[0]) / 9;
        assert!(
            mean_gap.abs_diff(DETECTION_INTERVAL) < Duration::from_millis(150),
            "{mean_gap:?}"
        );
    }

    #[test]
    fn slow_scans_never_redact_a_not_streaming_result() {
        let mut cache = cache(true, false);
        let completed_at = cache.completed_at.unwrap();
        // The next scan takes 40 s: far longer than the period, shorter than
        // the backstop. Gates keep the not-streaming answer throughout.
        let generation = claim_detection_refresh(&mut cache).unwrap();
        let finished_at = completed_at + Duration::from_secs(40);
        let mut now = completed_at;
        while now <= finished_at {
            assert!(!auto_detection_active(&cache, now), "redacted at {now:?}");
            now += Duration::from_millis(500);
        }
        assert_eq!(
            record_detection_into(&mut cache, not_streaming(), generation, finished_at),
            recorded(false, false),
            "nothing changed, so nothing is relaid out or broadcast"
        );
    }

    #[test]
    fn the_backstop_trips_only_after_its_full_duration_without_completions() {
        let cache = cache(true, false);
        let completed_at = cache.completed_at.unwrap();
        assert!(!auto_detection_active(
            &cache,
            completed_at + DETECTION_BACKSTOP
        ));
        assert!(!cache.untrusted_observed.load(Ordering::Relaxed));
        // The task died or a wake was missed: fail closed and note it.
        let later = completed_at + DETECTION_BACKSTOP + Duration::from_millis(1);
        assert!(auto_detection_active(&cache, later));
        assert!(cache.untrusted_observed.load(Ordering::Relaxed));
    }

    #[test]
    fn switching_into_auto_or_waking_fails_closed_until_a_new_generation_scan_completes() {
        // set_streamer_mode_status, update_streamer_mode_settings and
        // note_system_wake all reset the cache this way.
        let mut cache = cache(true, false);
        let now = cache.completed_at.unwrap();
        assert!(!auto_detection_active(&cache, now));

        reset_detection_for_auto(&mut cache);
        assert!(
            auto_detection_active(&cache, now),
            "an old not-streaming result must not unredact a switch into Auto or a wake"
        );
        let later = now + Duration::from_millis(50);
        assert_eq!(
            scan(&mut cache, not_streaming(), later),
            recorded(true, true),
            "the gate that failed closed above forces a rebuild"
        );
        assert!(!auto_detection_active(&cache, later));
        assert!(!cache.refresh_in_flight);
    }

    #[test]
    fn a_settings_replacement_into_auto_leaves_detection_pending() {
        // Import and reset to defaults call this under the settings lock.
        let mut cache = cache(true, false);
        let now = cache.completed_at.unwrap();
        let always_on = StreamerModeSettings {
            enabled: StreamerModeStatus::AlwaysOn,
            ..Default::default()
        };
        assert!(!reset_detection_for_replacement(
            &mut cache,
            Some(&always_on)
        ));
        assert!(
            cache.completed_at.is_some(),
            "leaving Auto keeps the result"
        );

        // Defaults (no streamer settings) select Auto, as an imported Auto does.
        for replacement in [None, Some(&StreamerModeSettings::default())] {
            let mut cache = self::cache(true, false);
            let stale = claim_detection_refresh(&mut cache).unwrap();
            assert!(reset_detection_for_replacement(&mut cache, replacement));
            assert!(cache.completed_at.is_none(), "detection is pending");
            assert!(auto_detection_active(&cache, now));
            assert_eq!(
                record_detection_into(&mut cache, not_streaming(), stale, now),
                DetectionOutcome::Discarded,
                "a scan from before the replacement is not trusted"
            );
        }
    }

    #[test]
    fn a_scan_in_flight_across_a_reset_is_discarded() {
        let mut cache = cache(true, false);
        let now = cache.completed_at.unwrap();
        let stale = claim_detection_refresh(&mut cache).unwrap();
        reset_detection_for_auto(&mut cache);
        assert_eq!(
            claim_detection_refresh(&mut cache),
            None,
            "the old scan still runs"
        );

        let finished_at = now + Duration::from_secs(1);
        assert_eq!(
            record_detection_into(&mut cache, not_streaming(), stale, finished_at),
            DetectionOutcome::Discarded
        );
        assert!(cache.completed_at.is_none());
        assert!(auto_detection_active(&cache, finished_at));

        let fresh = claim_detection_refresh(&mut cache).expect("discarding released the claim");
        assert_ne!(fresh, stale);
        assert!(matches!(
            record_detection_into(&mut cache, not_streaming(), fresh, finished_at),
            DetectionOutcome::Recorded { .. }
        ));
        assert!(!auto_detection_active(&cache, finished_at));
    }

    #[test]
    fn detections_broadcast_only_on_change_or_an_observed_untrusted_period() {
        let now = Instant::now();
        let mut steady = cache(true, false);
        assert_eq!(
            scan(&mut steady, not_streaming(), now),
            recorded(false, false)
        );
        let obs = || DetectionResult {
            active: true,
            source: Some("OBS is streaming".to_string()),
        };
        assert_eq!(scan(&mut steady, obs(), now), recorded(false, true));
        assert_eq!(scan(&mut steady, obs(), now), recorded(false, false));

        // An untrusted period no gate read neither relays out nor broadcasts.
        let mut unobserved = cache(true, false);
        reset_detection_for_auto(&mut unobserved);
        assert_eq!(
            scan(&mut unobserved, not_streaming(), now),
            recorded(false, false)
        );

        // Failing closed while "streaming" was cached showed nothing new.
        let mut streaming = cache(true, true);
        streaming.last_source = obs().source;
        reset_detection_for_auto(&mut streaming);
        assert!(auto_detection_active(&streaming, now));
        assert_eq!(scan(&mut streaming, obs(), now), recorded(false, false));

        // Startup: a gate read before the first result rebuilds after it.
        let mut startup = cache(false, false);
        assert!(auto_detection_active(&startup, now));
        assert_eq!(
            scan(&mut startup, not_streaming(), now),
            recorded(true, true)
        );
        assert!(!startup.untrusted_observed.load(Ordering::Relaxed));
    }

    #[test]
    fn a_panicking_detection_releases_its_claim_without_completing() {
        let lock = RwLock::new(cache(false, false));
        assert!(claim_detection_refresh(&mut lock.write().unwrap()).is_some());

        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            run_claimed_detection(&lock, || panic!("simulated detection panic"))
        }));
        assert!(outcome.is_err());

        let mut cache = lock.write().unwrap();
        assert!(!cache.refresh_in_flight, "a later scan can be claimed");
        assert!(cache.completed_at.is_none(), "the panic is not a detection");
        assert!(auto_detection_active(&cache, Instant::now()));
        assert!(claim_detection_refresh(&mut cache).is_some());
    }

    #[test]
    fn detection_refresh_is_claimed_once_at_a_time() {
        let mut cache = cache(true, false);
        let generation = claim_detection_refresh(&mut cache).unwrap();
        assert_eq!(
            claim_detection_refresh(&mut cache),
            None,
            "already in flight"
        );
        record_detection_into(&mut cache, not_streaming(), generation, Instant::now());
        assert_eq!(
            claim_detection_refresh(&mut cache),
            Some(generation),
            "a just-finished result does not refuse the next tick"
        );
    }

    #[test]
    fn a_panic_in_the_detection_loop_lets_the_next_gate_read_start_it_again() {
        static RUNNING: AtomicBool = AtomicBool::new(false);
        fn detect() -> DetectionResult {
            DetectionResult::default()
        }
        let cache: &'static RwLock<DetectionCache> =
            Box::leak(Box::new(RwLock::new(cache(false, false))));
        let scan_now: &'static tokio::sync::Notify =
            Box::leak(Box::new(tokio::sync::Notify::new()));
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();

        // start_detection_task claims the flag, then the task holds the guard.
        assert!(!RUNNING.swap(true, Ordering::AcqRel));
        let task = runtime.spawn(async move {
            let _running = DetectionTaskRunning(&RUNNING);
            run_detection_loop(
                cache,
                scan_now,
                Duration::from_millis(10),
                || true,
                detect,
                |_| panic!("simulated panic outside the scan"),
            )
            .await;
        });
        scan_now.notify_one();
        let joined = runtime.block_on(task);
        assert!(joined.is_err_and(|error| error.is_panic()));

        assert!(
            !RUNNING.load(Ordering::Acquire),
            "the panic cleared the running flag"
        );
        assert!(
            !RUNNING.swap(true, Ordering::AcqRel),
            "the next gate read can start the task again"
        );
        assert!(!cache.read().unwrap().refresh_in_flight);
    }

    #[test]
    fn the_detection_loop_scans_on_every_tick_and_survives_a_panicking_scan() {
        static SCANS: AtomicUsize = AtomicUsize::new(0);
        static SCANNING: AtomicBool = AtomicBool::new(false);
        fn detect() -> DetectionResult {
            assert!(!SCANNING.swap(true, Ordering::SeqCst), "two scans at once");
            let scan = SCANS.fetch_add(1, Ordering::SeqCst);
            std::thread::sleep(Duration::from_millis(3));
            SCANNING.store(false, Ordering::SeqCst);
            assert_ne!(scan, 1, "simulated detection panic");
            DetectionResult::default()
        }

        let cache: &'static RwLock<DetectionCache> =
            Box::leak(Box::new(RwLock::new(cache(false, false))));
        let scan_now = tokio::sync::Notify::new();
        let checks = AtomicUsize::new(0);
        let outcomes = AtomicUsize::new(0);
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();
        runtime.block_on(run_detection_loop(
            cache,
            &scan_now,
            Duration::from_millis(10),
            || {
                checks.fetch_add(1, Ordering::SeqCst);
                outcomes.load(Ordering::SeqCst) < 5
            },
            detect,
            |outcome| {
                assert_ne!(outcome, DetectionOutcome::Discarded);
                outcomes.fetch_add(1, Ordering::SeqCst);
            },
        ));

        let scans = SCANS.load(Ordering::SeqCst);
        assert_eq!(outcomes.load(Ordering::SeqCst), 5);
        assert_eq!(scans, 6, "the panicking scan did not stop the loop");
        assert_eq!(
            checks.load(Ordering::SeqCst),
            scans + 1,
            "every tick but the stopping one ran a scan"
        );
        let cache = cache.read().unwrap();
        assert!(!cache.refresh_in_flight);
        assert!(cache.completed_at.is_some());
    }
}
