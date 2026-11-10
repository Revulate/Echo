//! Opt-in, hermetic storage roots for repeatable benchmark observations.
//!
//! `ECHO_BENCH_PROFILE_ROOT` intentionally moves only Echo-owned state.  It
//! never changes OS-wide discovery paths: integrations that inspect another
//! application's data are disabled by their callers while a profile is active.

use serde::Serialize;
use std::{
    env, fs,
    io::{self, Read, Write},
    path::{Component, Path, PathBuf},
    sync::{
        OnceLock,
        atomic::{AtomicU64, Ordering},
    },
};

const PROFILE_ROOT_ENV: &str = "ECHO_BENCH_PROFILE_ROOT";
const RECEIPT_ENV: &str = "ECHO_BENCH_PROFILE_RECEIPT";
const NONCE_ENV: &str = "ECHO_BENCH_OBSERVATION_NONCE";
const TARGET_COMMIT_ENV: &str = "ECHO_BENCH_TARGET_COMMIT";
const OFFLINE_ENV: &str = "ECHO_BENCH_OFFLINE";
const LIVE_CHANNEL_ENV: &str = "ECHO_BENCH_LIVE_CHANNEL";
const LIVE_CHANNEL_RECEIPT_ENV: &str = "ECHO_BENCH_LIVE_CHANNEL_RECEIPT";
const LIVE_CHANNEL_READY_TIMEOUT_SECS: u64 = 20;
const LINK_PREVIEW_MATRIX_READY_TIMEOUT_SECS: u64 = 90;

#[derive(Clone, Debug)]
struct Roots {
    profile: PathBuf,
    config: PathBuf,
    data: PathBuf,
    cache: PathBuf,
    logs: PathBuf,
}

impl Roots {
    fn from_profile(profile: PathBuf) -> Self {
        Self {
            config: profile.join("config"),
            data: profile.join("data"),
            cache: profile.join("cache"),
            logs: profile.join("logs"),
            profile,
        }
    }

    fn create_all(&self) -> io::Result<()> {
        for root in [
            &self.profile,
            &self.config,
            &self.data,
            &self.cache,
            &self.logs,
        ] {
            fs::create_dir_all(root)?;
        }
        for directory in ["cache", "history", "logs", "native-messaging"] {
            fs::create_dir_all(self.config.join(directory))?;
        }
        fs::create_dir_all(self.cache.join("updates"))?;
        Ok(())
    }
}

static PROFILE_ROOTS: OnceLock<Option<Roots>> = OnceLock::new();
static LIVE_CHANNEL_BENCHMARK: OnceLock<Option<LiveChannelBenchmark>> = OnceLock::new();
static LIVE_OUTBOUND_USER_ACTION_ATTEMPTS: AtomicU64 = AtomicU64::new(0);
static LIVE_INBOUND_PRIVMSG_COUNT: AtomicU64 = AtomicU64::new(0);
static RECEIPT_TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// The explicit, no-send live-channel workload used by matched resource
/// observations. It is deliberately separate from the synthetic UI
/// fixture: the selected channel is opened through the ordinary workspace and
/// connection lifecycle.
#[derive(Clone, Debug)]
struct LiveChannelBenchmark {
    channel: String,
    receipt_path: PathBuf,
    receipt_parent: PathBuf,
    observation_nonce: String,
    profile_root: PathBuf,
}

/// Initialize the optional profile before normal application startup.
///
/// A configured profile root is required to be absolute. This fails closed:
/// an invalid benchmark configuration cannot fall through to a user's normal
/// settings, tokens, or caches.
pub fn initialize() -> Result<(), String> {
    let roots = PROFILE_ROOTS.get_or_init(|| {
        let raw = env::var_os(PROFILE_ROOT_ENV)?;
        let profile = PathBuf::from(raw);
        Some(Roots::from_profile(profile))
    });

    let Some(roots) = roots.as_ref() else {
        if live_channel_environment_is_configured() {
            return Err(format!(
                "{LIVE_CHANNEL_ENV} requires {PROFILE_ROOT_ENV} so live benchmark state cannot use normal Echo storage"
            ));
        }
        let _ = LIVE_CHANNEL_BENCHMARK.set(None);
        return Ok(());
    };
    if !roots.profile.is_absolute() {
        return Err(format!("{PROFILE_ROOT_ENV} must be an absolute path"));
    }
    roots.create_all().map_err(|error| {
        format!(
            "failed to create benchmark profile roots under '{}': {error}",
            roots.profile.display()
        )
    })?;
    let live_channel = parse_live_channel_benchmark(roots)?;
    let _ = LIVE_CHANNEL_BENCHMARK.set(live_channel);
    write_receipt(roots)
}

/// The normalized Twitch login requested for the benchmark startup workspace.
/// This is `None` for every ordinary Echo launch.
pub fn live_channel() -> Option<&'static str> {
    LIVE_CHANNEL_BENCHMARK
        .get()
        .and_then(Option::as_ref)
        .map(|benchmark| benchmark.channel.as_str())
}

/// Reject a user-originating outbound action during any no-send benchmark.
/// Transport protocol traffic (IRC JOIN/PONG and connection supervision) never
/// calls this boundary. The counter is process-local and exists while either the
/// explicit live-channel contract or the isolated offline guard is active.
pub fn reject_live_outbound_user_action(action: &str) -> Result<(), String> {
    if !outbound_user_action_must_be_rejected(live_channel().is_some(), network_lanes_suppressed())
    {
        return Ok(());
    }

    let attempt = LIVE_OUTBOUND_USER_ACTION_ATTEMPTS.fetch_add(1, Ordering::AcqRel) + 1;
    Err(format!(
        "Benchmark no-send guard rejected outbound user action '{action}' (attempt {attempt})"
    ))
}

fn outbound_user_action_must_be_rejected(
    live_channel_active: bool,
    network_lanes_suppressed: bool,
) -> bool {
    live_channel_active || network_lanes_suppressed
}

pub fn live_outbound_user_action_attempts() -> u64 {
    LIVE_OUTBOUND_USER_ACTION_ATTEMPTS.load(Ordering::Acquire)
}

/// Record one deduplicated, visible Twitch IRC PRIVMSG accepted by Echo's
/// ordinary live connection path. History hydration deliberately does not call
/// this boundary, so benchmark readiness cannot be satisfied by an 800-row
/// recent-message backfill alone.
pub fn note_live_inbound_privmsg(channel: &str) {
    if live_channel().is_some_and(|requested| requested.eq_ignore_ascii_case(channel)) {
        LIVE_INBOUND_PRIVMSG_COUNT.fetch_add(1, Ordering::AcqRel);
    }
}

pub fn live_inbound_privmsg_count() -> u64 {
    LIVE_INBOUND_PRIVMSG_COUNT.load(Ordering::Acquire)
}

/// Start the terminal canonical readiness receipt after the main GPUI workspace
/// has been created. The probe never injects input or invokes a send API; it
/// only observes the normal lifecycle's public connection state. It publishes
/// once, when the live workload is ready or the bounded deadline expires.
pub fn start_live_channel_readiness_probe(
    startup_active_split_channel: Option<String>,
    startup_active_tab_channels: Vec<String>,
) -> Result<(), String> {
    let Some(benchmark) = LIVE_CHANNEL_BENCHMARK
        .get()
        .and_then(Option::as_ref)
        .cloned()
    else {
        return Ok(());
    };

    std::thread::Builder::new()
        .name("bench-live-channel-readiness".to_string())
        .spawn(move || {
            let ready_timeout_secs = if crate::benchmark_ui_fixture::link_preview_matrix_enabled() {
                LINK_PREVIEW_MATRIX_READY_TIMEOUT_SECS
            } else {
                LIVE_CHANNEL_READY_TIMEOUT_SECS
            };
            let deadline =
                std::time::Instant::now() + std::time::Duration::from_secs(ready_timeout_secs);
            loop {
                let result = write_live_channel_receipt(
                    &benchmark,
                    startup_active_split_channel.as_deref(),
                    &startup_active_tab_channels,
                    std::time::Instant::now() >= deadline,
                );
                match result {
                    Ok(true) => return,
                    Ok(false) if std::time::Instant::now() >= deadline => {
                        match write_live_channel_receipt(
                            &benchmark,
                            startup_active_split_channel.as_deref(),
                            &startup_active_tab_channels,
                            true,
                        ) {
                            Ok(_) => return,
                            Err(error) => {
                                tracing::error!(%error, "Live benchmark terminal receipt failed");
                                std::process::exit(1);
                            }
                        }
                    }
                    Ok(false) => std::thread::sleep(std::time::Duration::from_millis(200)),
                    Err(error) => {
                        tracing::error!(%error, "Live benchmark readiness receipt failed");
                        std::process::exit(1);
                    }
                }
            }
        })
        .map(|_| ())
        .map_err(|error| format!("failed to start live benchmark readiness probe: {error}"))
}

/// Whether this process is isolated from the user's normal Echo storage.
pub fn is_enabled() -> bool {
    PROFILE_ROOTS.get().and_then(Option::as_ref).is_some()
        || env::var_os(PROFILE_ROOT_ENV).is_some()
}

pub fn profile_root() -> Option<PathBuf> {
    profile_roots().map(|roots| roots.profile.clone())
}

/// Return whether every application-owned network lane must remain disabled.
///
/// `ECHO_BENCH_OFFLINE` is accepted only with an isolated benchmark profile,
/// so an ordinary user launch cannot accidentally inherit offline behavior.
pub fn network_lanes_suppressed() -> bool {
    offline_env_suppresses_network(
        profile_root().is_some(),
        env::var(OFFLINE_ENV).ok().as_deref(),
    )
}

fn offline_env_suppresses_network(profile_active: bool, value: Option<&str>) -> bool {
    profile_active
        && value.is_some_and(|value| matches!(value, "1" | "true" | "TRUE" | "yes" | "YES"))
}

pub fn config_root() -> Option<PathBuf> {
    profile_roots()
        .map(|roots| roots.config.clone())
        .or_else(|| dirs::config_dir().map(|path| path.join("echo")))
}

pub fn config_root_or_current_dir() -> PathBuf {
    config_root().unwrap_or_else(|| {
        env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join("echo")
    })
}

/// Preserve the one historical caller whose fallback was the working directory
/// itself rather than a dedicated `echo` directory.
pub fn config_root_or_dot() -> PathBuf {
    config_root().unwrap_or_else(|| PathBuf::from("."))
}

pub fn data_root() -> Option<PathBuf> {
    profile_roots()
        .map(|roots| roots.data.clone())
        .or_else(|| dirs::data_local_dir().map(|path| path.join("echo")))
}

pub fn cache_root() -> Option<PathBuf> {
    profile_roots()
        .map(|roots| roots.cache.clone())
        .or_else(|| dirs::cache_dir().map(|path| path.join("echo")))
}

/// Runtime log root. In a profile it is separate from all other state;
/// ordinary installs retain their existing local-data log location.
pub fn log_root() -> Option<PathBuf> {
    profile_roots()
        .map(|roots| roots.logs.clone())
        .or_else(data_root)
}

/// Per-user runtime directory for the Linux single-instance lock, its
/// activation socket, and short-lived desktop-integration scripts. A profile
/// keeps its own, so a campaign never collides with (or raises) the user's
/// Echo; ordinary installs use `XDG_RUNTIME_DIR`, falling back to the
/// local-data root where it is unset.
pub fn runtime_root() -> Option<PathBuf> {
    profile_roots()
        .map(|roots| roots.data.join("runtime"))
        .or_else(|| dirs::runtime_dir().map(|path| path.join("echo")))
        .or_else(data_root)
}

/// The OS configuration root for *other applications*. Benchmark profiles must
/// not point OBS/Discord discovery at synthetic Echo directories.
pub fn external_config_dir() -> Option<PathBuf> {
    (!is_enabled()).then(dirs::config_dir).flatten()
}

/// The OS per-user application data root for *other applications*
/// (`%APPDATA%`, `~/.local/share`, `~/Library/Application Support`), where the
/// settings importer looks for the install it imports from. `None` while a
/// benchmark profile is active, so a campaign never reads the user's real
/// settings.
pub fn external_data_dir() -> Option<PathBuf> {
    (!is_enabled()).then(dirs::data_dir).flatten()
}

/// A historical cwd file is readable only outside a benchmark profile.
pub fn legacy_cwd_file(name: &str) -> Option<PathBuf> {
    (!is_enabled()).then(|| PathBuf::from(name))
}

/// Emote preferences predate the main settings store and historically lived
/// under `HOME/.config/echo`; preserve that normal location while isolating a
/// campaign in its config root.
pub fn emote_preferences_path() -> PathBuf {
    if is_enabled() {
        return config_root_or_current_dir().join("emote_preferences.json");
    }
    let home = env::var("HOME")
        .or_else(|_| env::var("USERPROFILE"))
        .unwrap_or_else(|_| ".".to_string());
    PathBuf::from(home)
        .join(".config")
        .join("echo")
        .join("emote_preferences.json")
}

/// Sound settings live in the config root; `SoundStore` migrates the
/// historical working-directory file via `legacy_cwd_file("sounds.json")`.
pub fn sound_settings_path() -> PathBuf {
    config_root_or_current_dir().join("sounds.json")
}

/// Profiling artifacts use the profile log root when isolation is enabled and
/// retain their cwd filename in ordinary runs.
pub fn profiling_artifact_path(name: &str) -> PathBuf {
    if is_enabled() {
        log_root().unwrap_or_else(|| PathBuf::from(".")).join(name)
    } else {
        PathBuf::from(name)
    }
}

fn profile_roots() -> Option<&'static Roots> {
    PROFILE_ROOTS.get().and_then(Option::as_ref)
}

#[derive(Serialize)]
struct Receipt<'a> {
    schema_version: u32,
    accepted: bool,
    process_id: u32,
    executable_path: PathBuf,
    executable_sha256: String,
    executable_size_bytes: u64,
    emitted_at_utc: String,
    keyring_isolated: bool,
    external_discovery_disabled: bool,
    profile_root: &'a Path,
    config_root: &'a Path,
    data_root: &'a Path,
    cache_root: &'a Path,
    log_root: &'a Path,
    #[serde(skip_serializing_if = "Option::is_none")]
    observation_nonce: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    target_commit: Option<String>,
    roots: ReceiptRoots<'a>,
}

#[derive(Serialize)]
struct ReceiptRoots<'a> {
    settings_config: PathBuf,
    history_logs: PathBuf,
    media_disk_cache: PathBuf,
    warm_cache: PathBuf,
    update_cache: PathBuf,
    reliability_upload_user_notes: PathBuf,
    browser_watching: PathBuf,
    irc_colors: PathBuf,
    profiling: &'a Path,
    emote_preferences: PathBuf,
    sound_settings: PathBuf,
    trace_profile: PathBuf,
    dhat_profile: PathBuf,
}

fn write_receipt(roots: &Roots) -> Result<(), String> {
    let path = env::var_os(RECEIPT_ENV)
        .map(PathBuf::from)
        .unwrap_or_else(|| roots.profile.join("benchmark-profile-roots.json"));
    if !path.is_absolute() {
        return Err(format!("{RECEIPT_ENV} must be an absolute path"));
    }
    let executable_path = env::current_exe()
        .map_err(|error| format!("failed to resolve benchmark executable: {error}"))?
        .canonicalize()
        .map_err(|error| format!("failed to canonicalize benchmark executable: {error}"))?;
    let executable_size_bytes = fs::metadata(&executable_path)
        .map_err(|error| format!("failed to inspect benchmark executable: {error}"))?
        .len();
    let executable_sha256 = sha256_file(&executable_path)?;
    let receipt = Receipt {
        schema_version: 1,
        accepted: true,
        process_id: std::process::id(),
        executable_path: receipt_path(&executable_path),
        executable_sha256,
        executable_size_bytes,
        emitted_at_utc: canonical_utc_timestamp(chrono::Utc::now()),
        keyring_isolated: true,
        external_discovery_disabled: true,
        profile_root: &roots.profile,
        config_root: &roots.config,
        data_root: &roots.data,
        cache_root: &roots.cache,
        log_root: &roots.logs,
        observation_nonce: env::var(NONCE_ENV).ok().filter(|value| !value.is_empty()),
        target_commit: env::var(TARGET_COMMIT_ENV)
            .ok()
            .filter(|value| !value.is_empty()),
        roots: ReceiptRoots {
            settings_config: roots.config.clone(),
            history_logs: roots.config.join("logs"),
            media_disk_cache: roots.config.join("cache"),
            warm_cache: roots.config.join("warming-priorities.bin"),
            update_cache: roots.cache.join("updates"),
            reliability_upload_user_notes: roots.config.clone(),
            browser_watching: roots.config.join("native-messaging"),
            irc_colors: roots.config.clone(),
            profiling: &roots.logs,
            emote_preferences: roots.config.join("emote_preferences.json"),
            sound_settings: roots.config.join("sounds.json"),
            trace_profile: roots.logs.join("echo_trace.json"),
            dhat_profile: roots.logs.join("dhat-heap.json"),
        },
    };
    let bytes = serde_json::to_vec_pretty(&receipt)
        .map_err(|error| format!("failed to serialize benchmark profile receipt: {error}"))?;
    let parent = path.parent().ok_or_else(|| {
        format!(
            "benchmark profile receipt '{}' has no parent",
            path.display()
        )
    })?;
    fs::create_dir_all(parent).map_err(|error| {
        format!(
            "failed to create benchmark profile receipt directory '{}': {error}",
            parent.display()
        )
    })?;
    let temporary = path.with_extension("json.tmp");
    fs::write(&temporary, bytes).map_err(|error| {
        format!(
            "failed to write benchmark profile receipt '{}': {error}",
            temporary.display()
        )
    })?;
    if let Err(error) = crate::utils::replace_file(&temporary, &path) {
        let _ = fs::remove_file(&temporary);
        return Err(format!(
            "failed to atomically publish benchmark profile receipt '{}': {error}",
            path.display()
        ));
    }
    Ok(())
}

fn live_channel_environment_is_configured() -> bool {
    env::var_os(LIVE_CHANNEL_ENV).is_some() || env::var_os(LIVE_CHANNEL_RECEIPT_ENV).is_some()
}

fn parse_live_channel_benchmark(roots: &Roots) -> Result<Option<LiveChannelBenchmark>, String> {
    let Some(raw_channel) = env::var_os(LIVE_CHANNEL_ENV) else {
        if env::var_os(LIVE_CHANNEL_RECEIPT_ENV).is_some() {
            return Err(format!(
                "{LIVE_CHANNEL_RECEIPT_ENV} requires {LIVE_CHANNEL_ENV}"
            ));
        }
        return Ok(None);
    };

    if crate::benchmark_ui_fixture::is_enabled() {
        return Err(format!(
            "{LIVE_CHANNEL_ENV} cannot be combined with ECHO_BENCH_UI_FIXTURE; live observations must use the normal channel lifecycle"
        ));
    }

    let raw_channel = raw_channel.to_string_lossy();
    let channel = normalize_twitch_channel(&raw_channel)?;
    let receipt_path = env::var_os(LIVE_CHANNEL_RECEIPT_ENV)
        .map(PathBuf::from)
        .ok_or_else(|| {
            format!("{LIVE_CHANNEL_RECEIPT_ENV} is required when {LIVE_CHANNEL_ENV} is set")
        })?;
    let profile_root = normalize_absolute_path(&roots.profile, PROFILE_ROOT_ENV)?;
    let receipt_path = normalize_absolute_path(&receipt_path, LIVE_CHANNEL_RECEIPT_ENV)?;
    let relative_receipt = receipt_path.strip_prefix(&profile_root).map_err(|_| {
        format!(
            "{LIVE_CHANNEL_RECEIPT_ENV} '{}' must be contained under {PROFILE_ROOT_ENV} '{}'",
            receipt_path.display(),
            profile_root.display()
        )
    })?;
    if relative_receipt.as_os_str().is_empty() {
        return Err(format!(
            "{LIVE_CHANNEL_RECEIPT_ENV} must name a file beneath {PROFILE_ROOT_ENV}"
        ));
    }
    let receipt_parent = receipt_path.parent().ok_or_else(|| {
        format!(
            "{LIVE_CHANNEL_RECEIPT_ENV} '{}' has no parent directory",
            receipt_path.display()
        )
    })?;
    fs::create_dir_all(receipt_parent).map_err(|error| {
        format!(
            "failed to create {LIVE_CHANNEL_RECEIPT_ENV} parent '{}': {error}",
            receipt_parent.display()
        )
    })?;
    let canonical_profile_root = profile_root.canonicalize().map_err(|error| {
        format!(
            "failed to canonicalize {PROFILE_ROOT_ENV} '{}': {error}",
            profile_root.display()
        )
    })?;
    let canonical_receipt_parent = receipt_parent.canonicalize().map_err(|error| {
        format!(
            "failed to canonicalize {LIVE_CHANNEL_RECEIPT_ENV} parent '{}': {error}",
            receipt_parent.display()
        )
    })?;
    if !canonical_receipt_parent.starts_with(&canonical_profile_root) {
        return Err(format!(
            "{LIVE_CHANNEL_RECEIPT_ENV} parent '{}' escapes canonical {PROFILE_ROOT_ENV} '{}'",
            canonical_receipt_parent.display(),
            canonical_profile_root.display()
        ));
    }
    let receipt_name = receipt_path.file_name().ok_or_else(|| {
        format!(
            "{LIVE_CHANNEL_RECEIPT_ENV} '{}' must name a receipt file",
            receipt_path.display()
        )
    })?;
    let receipt_path = canonical_receipt_parent.join(receipt_name);
    reject_reparse_destination(&receipt_path, LIVE_CHANNEL_RECEIPT_ENV)?;
    let observation_nonce = env::var(NONCE_ENV)
        .ok()
        .filter(|nonce| !nonce.trim().is_empty())
        .ok_or_else(|| format!("{NONCE_ENV} is required for a live-channel benchmark"))?;

    Ok(Some(LiveChannelBenchmark {
        channel,
        receipt_path,
        receipt_parent: canonical_receipt_parent,
        observation_nonce,
        profile_root: canonical_profile_root,
    }))
}

fn reject_reparse_destination(path: &Path, name: &str) -> Result<(), String> {
    let Ok(metadata) = fs::symlink_metadata(path) else {
        return Ok(());
    };
    if metadata.file_type().is_symlink() || metadata_is_reparse_point(&metadata) {
        return Err(format!(
            "{name} '{}' must not target a symlink or reparse point",
            path.display()
        ));
    }
    Ok(())
}

#[cfg(windows)]
fn metadata_is_reparse_point(metadata: &fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;

    metadata.file_attributes() & 0x400 != 0
}

#[cfg(not(windows))]
fn metadata_is_reparse_point(_metadata: &fs::Metadata) -> bool {
    false
}

/// Normalize an absolute path without resolving symlinks. Live benchmark
/// receipts must never use `..`; this keeps containment validation deterministic
/// before the receipt directory exists and rejects attempts to escape the
/// profile tree by parent traversal.
fn normalize_absolute_path(path: &Path, environment_name: &str) -> Result<PathBuf, String> {
    if !path.is_absolute() {
        return Err(format!("{environment_name} must be an absolute path"));
    }

    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            Component::RootDir => normalized.push(component.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                return Err(format!(
                    "{environment_name} must not contain parent-directory traversal"
                ));
            }
            Component::Normal(segment) => normalized.push(segment),
        }
    }

    if normalized.is_absolute() {
        Ok(normalized)
    } else {
        Err(format!("{environment_name} must be an absolute path"))
    }
}

fn normalize_twitch_channel(raw_channel: &str) -> Result<String, String> {
    let channel = raw_channel
        .trim()
        .trim_start_matches('#')
        .to_ascii_lowercase();
    let valid = (4..=25).contains(&channel.len())
        && channel
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_');
    if valid {
        Ok(channel)
    } else {
        Err(format!(
            "{LIVE_CHANNEL_ENV} must be a Twitch login of 4-25 ASCII letters, digits, or underscores"
        ))
    }
}

#[derive(Serialize)]
struct LiveChannelReceipt<'a> {
    schema_version: u32,
    accepted: bool,
    ready: bool,
    timed_out: bool,
    process_id: u32,
    executable_path: PathBuf,
    executable_sha256: String,
    executable_size_bytes: u64,
    emitted_at_utc: String,
    observation_nonce: &'a str,
    profile_root: PathBuf,
    requested_channel: &'a str,
    normalized_channel: &'a str,
    startup_active_split_channel: Option<&'a str>,
    startup_active_split_matches_requested: bool,
    startup_active_tab_channels: &'a [String],
    startup_active_tab_has_only_requested_channel: bool,
    transport_connection_status: &'static str,
    /// Echo's normal IRC membership tracker has accepted the channel request.
    /// This does not claim that Twitch has sent a protocol-level JOIN echo.
    irc_membership_tracked: bool,
    lifecycle_connected: bool,
    /// Twitch ROOMSTATE has been received and retained for the requested
    /// channel, proving server-observed channel state reached Echo.
    roomstate_observed: bool,
    /// Raw diagnostic count from Echo's normal bounded per-channel buffer.
    message_count: usize,
    /// Excludes system rows but may include recent-message history hydration;
    /// retained as a diagnostic rather than the live-ingress readiness gate.
    renderable_message_count: usize,
    /// Deduplicated PRIVMSG rows accepted by the ordinary live IRC handler.
    /// History hydration never increments this counter.
    live_privmsg_count: u64,
    messages_observed: bool,
    outbound_user_action_attempts: u64,
    send_attempted: bool,
}

fn write_live_channel_receipt(
    benchmark: &LiveChannelBenchmark,
    startup_active_split_channel: Option<&str>,
    startup_active_tab_channels: &[String],
    timed_out: bool,
) -> Result<bool, String> {
    let startup_active_split_matches_requested = startup_active_split_channel
        .map(|channel| channel.eq_ignore_ascii_case(&benchmark.channel))
        .unwrap_or(false);
    let startup_active_tab_has_only_requested_channel = startup_active_tab_channels.len() == 1
        && startup_active_tab_channels[0].eq_ignore_ascii_case(&benchmark.channel);
    let irc_membership_tracked = crate::backend::irc::is_channel_connected(&benchmark.channel);
    let lifecycle_connected =
        crate::backend::channel_lifecycle::get_lifecycle_manager().is_connected(&benchmark.channel);
    let (message_count, renderable_message_count) = crate::backend::irc::CHAT_MESSAGES
        .get(&benchmark.channel)
        .map(|messages| {
            let message_count = messages.len();
            let renderable_message_count = messages
                .iter()
                .filter(|message| !message.flags.is_system)
                .count();
            (message_count, renderable_message_count)
        })
        .unwrap_or((0, 0));
    let roomstate_observed =
        crate::backend::irc::room_state::get_room_state(&benchmark.channel).is_some();
    let live_privmsg_count = live_inbound_privmsg_count();
    let messages_observed = live_privmsg_count > 0;
    let outbound_user_action_attempts = live_outbound_user_action_attempts();
    let transport_connection_status = match crate::backend::irc::get_connection_status() {
        crate::backend::irc::ConnectionStatus::Connected => "connected",
        crate::backend::irc::ConnectionStatus::Reconnecting => "reconnecting",
        crate::backend::irc::ConnectionStatus::Disconnected => "disconnected",
    };
    let ready = startup_active_split_matches_requested
        && startup_active_tab_has_only_requested_channel
        && irc_membership_tracked
        && lifecycle_connected
        && transport_connection_status == "connected"
        && roomstate_observed
        && messages_observed
        && renderable_message_count > 0
        && outbound_user_action_attempts == 0;
    if !ready && !timed_out {
        return Ok(false);
    }
    let executable_path = env::current_exe()
        .map_err(|error| format!("failed to resolve live benchmark executable: {error}"))?
        .canonicalize()
        .map_err(|error| format!("failed to canonicalize live benchmark executable: {error}"))?;
    let executable_size_bytes = fs::metadata(&executable_path)
        .map_err(|error| format!("failed to inspect live benchmark executable: {error}"))?
        .len();
    let receipt = LiveChannelReceipt {
        schema_version: 2,
        accepted: true,
        ready,
        timed_out: timed_out && !ready,
        process_id: std::process::id(),
        executable_path: receipt_path(&executable_path),
        executable_sha256: sha256_file(&executable_path)?,
        executable_size_bytes,
        emitted_at_utc: canonical_utc_timestamp(chrono::Utc::now()),
        observation_nonce: &benchmark.observation_nonce,
        profile_root: receipt_path(&benchmark.profile_root),
        requested_channel: &benchmark.channel,
        normalized_channel: &benchmark.channel,
        startup_active_split_channel,
        startup_active_split_matches_requested,
        startup_active_tab_channels,
        startup_active_tab_has_only_requested_channel,
        transport_connection_status,
        irc_membership_tracked,
        lifecycle_connected,
        roomstate_observed,
        message_count,
        renderable_message_count,
        live_privmsg_count,
        messages_observed,
        outbound_user_action_attempts,
        send_attempted: outbound_user_action_attempts != 0,
    };
    write_json_receipt(
        &benchmark.receipt_path,
        &receipt,
        "live channel benchmark receipt",
        &benchmark.receipt_parent,
    )?;
    Ok(ready)
}

fn write_json_receipt(
    path: &Path,
    receipt: &impl Serialize,
    receipt_name: &str,
    verified_parent: &Path,
) -> Result<(), String> {
    let bytes = serde_json::to_vec_pretty(receipt)
        .map_err(|error| format!("failed to serialize {receipt_name}: {error}"))?;
    let parent = path
        .parent()
        .ok_or_else(|| format!("{receipt_name} '{}' has no parent", path.display()))?;
    let canonical_parent = parent.canonicalize().map_err(|error| {
        format!(
            "failed to canonicalize {receipt_name} parent '{}': {error}",
            parent.display()
        )
    })?;
    if canonical_parent != verified_parent {
        return Err(format!(
            "{receipt_name} parent '{}' changed after benchmark validation",
            canonical_parent.display()
        ));
    }
    reject_reparse_destination(path, receipt_name)?;
    let (temporary, mut file) = create_unique_receipt_temp(&canonical_parent, path, receipt_name)?;
    if let Err(error) = file.write_all(&bytes).and_then(|_| file.sync_all()) {
        let _ = fs::remove_file(&temporary);
        return Err(format!(
            "failed to write {receipt_name} '{}': {error}",
            temporary.display()
        ));
    }
    drop(file);
    reject_reparse_destination(path, receipt_name)?;
    if let Err(error) = crate::utils::replace_file(&temporary, path) {
        let _ = fs::remove_file(&temporary);
        return Err(format!(
            "failed to atomically publish {receipt_name} '{}': {error}",
            path.display()
        ));
    }
    Ok(())
}

fn create_unique_receipt_temp(
    parent: &Path,
    destination: &Path,
    receipt_name: &str,
) -> Result<(PathBuf, fs::File), String> {
    let file_name = destination.file_name().ok_or_else(|| {
        format!(
            "{receipt_name} '{}' has no file name",
            destination.display()
        )
    })?;
    for _ in 0..64 {
        let sequence = RECEIPT_TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let temporary = parent.join(format!(
            ".{}.{}.{}.tmp",
            file_name.to_string_lossy(),
            std::process::id(),
            sequence
        ));
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
        {
            Ok(file) => return Ok((temporary, file)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(format!(
                    "failed to create {receipt_name} temporary file '{}': {error}",
                    temporary.display()
                ));
            }
        }
    }
    Err(format!(
        "failed to reserve a unique {receipt_name} temporary file under '{}'",
        parent.display()
    ))
}

fn canonical_utc_timestamp(timestamp: chrono::DateTime<chrono::Utc>) -> String {
    format!(
        "{}.{:07}Z",
        timestamp.format("%Y-%m-%dT%H:%M:%S"),
        timestamp.timestamp_subsec_nanos() / 100
    )
}

fn receipt_path(path: &Path) -> PathBuf {
    #[cfg(windows)]
    {
        if let Some(path) = path.to_str().and_then(|path| path.strip_prefix(r"\\?\")) {
            return PathBuf::from(path);
        }
    }
    path.to_path_buf()
}

fn sha256_file(path: &Path) -> Result<String, String> {
    let mut file = fs::File::open(path)
        .map_err(|error| format!("failed to open benchmark executable for hashing: {error}"))?;
    let mut hash = ring::digest::Context::new(&ring::digest::SHA256);
    let mut buffer = [0_u8; 1024 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|error| format!("failed to hash benchmark executable: {error}"))?;
        if read == 0 {
            break;
        }
        hash.update(&buffer[..read]);
    }
    Ok(hash
        .finish()
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::{offline_env_suppresses_network, outbound_user_action_must_be_rejected};
    use core::prelude::v1::test;

    #[test]
    fn offline_and_live_benchmarks_reject_outbound_user_actions() {
        assert!(!outbound_user_action_must_be_rejected(false, false));
        assert!(outbound_user_action_must_be_rejected(true, false));
        assert!(outbound_user_action_must_be_rejected(false, true));
        assert!(outbound_user_action_must_be_rejected(true, true));
    }

    #[test]
    fn lightweight_offline_mode_requires_an_isolated_profile() {
        assert!(!offline_env_suppresses_network(false, Some("1")));
        assert!(!offline_env_suppresses_network(true, Some("false")));
        assert!(offline_env_suppresses_network(true, Some("1")));
        assert!(offline_env_suppresses_network(true, Some("yes")));
    }
}
