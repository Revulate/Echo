//! The explicit, no-send live-channel workload used by matched resource
//! observations (`ECHO_BENCH_LIVE_CHANNEL`). It is deliberately separate from
//! the synthetic UI fixture: the selected channel is opened through the
//! ordinary workspace and connection lifecycle, and a readiness probe publishes
//! one receipt once the workload is live or a bounded deadline expires.

use serde::Serialize;
use std::{
    env, fs,
    path::PathBuf,
    sync::{
        OnceLock,
        atomic::{AtomicU64, Ordering},
    },
};

use super::receipt::{
    NONCE_ENV, canonical_utc_timestamp, normalize_absolute_path, receipt_path,
    reject_reparse_destination, sha256_file, write_json_receipt,
};
use crate::storage_roots::{PROFILE_ROOT_ENV, ProfileRoots};

const LIVE_CHANNEL_ENV: &str = "ECHO_BENCH_LIVE_CHANNEL";
const LIVE_CHANNEL_RECEIPT_ENV: &str = "ECHO_BENCH_LIVE_CHANNEL_RECEIPT";
const LIVE_CHANNEL_READY_TIMEOUT_SECS: u64 = 20;
const LINK_PREVIEW_MATRIX_READY_TIMEOUT_SECS: u64 = 90;

static LIVE_CHANNEL_BENCHMARK: OnceLock<Option<LiveChannelBenchmark>> = OnceLock::new();
static LIVE_INBOUND_PRIVMSG_COUNT: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Debug)]
struct LiveChannelBenchmark {
    channel: String,
    receipt_path: PathBuf,
    receipt_parent: PathBuf,
    observation_nonce: String,
    profile_root: PathBuf,
}

/// Without an isolated profile the live-channel workload must not be
/// configured, so live benchmark state cannot use normal Echo storage.
pub(super) fn initialize_without_profile() -> Result<(), String> {
    if live_channel_environment_is_configured() {
        return Err(format!(
            "{LIVE_CHANNEL_ENV} requires {PROFILE_ROOT_ENV} so live benchmark state cannot use normal Echo storage"
        ));
    }
    let _ = LIVE_CHANNEL_BENCHMARK.set(None);
    Ok(())
}

pub(super) fn initialize(roots: &ProfileRoots) -> Result<(), String> {
    let live_channel = parse_live_channel_benchmark(roots)?;
    let _ = LIVE_CHANNEL_BENCHMARK.set(live_channel);
    Ok(())
}

/// The normalized Twitch login requested for the benchmark startup workspace.
/// This is `None` for every ordinary Echo launch.
pub fn requested_channel() -> Option<&'static str> {
    LIVE_CHANNEL_BENCHMARK
        .get()
        .and_then(Option::as_ref)
        .map(|benchmark| benchmark.channel.as_str())
}

/// Record one deduplicated, visible Twitch IRC PRIVMSG accepted by Echo's
/// ordinary live connection path. History hydration deliberately does not call
/// this boundary, so benchmark readiness cannot be satisfied by an 800-row
/// recent-message backfill alone.
pub fn note_inbound_privmsg(channel: &str) {
    if requested_channel().is_some_and(|requested| requested.eq_ignore_ascii_case(channel)) {
        LIVE_INBOUND_PRIVMSG_COUNT.fetch_add(1, Ordering::AcqRel);
    }
}

fn inbound_privmsg_count() -> u64 {
    LIVE_INBOUND_PRIVMSG_COUNT.load(Ordering::Acquire)
}

/// Start the terminal canonical readiness receipt after the main GPUI workspace
/// has been created. The probe never injects input or invokes a send API; it
/// only observes the normal lifecycle's public connection state. It publishes
/// once, when the live workload is ready or the bounded deadline expires.
pub fn start_readiness_probe(
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
            let ready_timeout_secs = if crate::bench::ui_fixture::link_preview_matrix_enabled() {
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

fn live_channel_environment_is_configured() -> bool {
    echo_core::env::os_value(LIVE_CHANNEL_ENV).is_some()
        || echo_core::env::os_value(LIVE_CHANNEL_RECEIPT_ENV).is_some()
}

fn parse_live_channel_benchmark(
    roots: &ProfileRoots,
) -> Result<Option<LiveChannelBenchmark>, String> {
    let Some(raw_channel) = echo_core::env::os_value(LIVE_CHANNEL_ENV) else {
        if echo_core::env::os_value(LIVE_CHANNEL_RECEIPT_ENV).is_some() {
            return Err(format!(
                "{LIVE_CHANNEL_RECEIPT_ENV} requires {LIVE_CHANNEL_ENV}"
            ));
        }
        return Ok(None);
    };

    if crate::bench::ui_fixture::is_enabled() {
        return Err(format!(
            "{LIVE_CHANNEL_ENV} cannot be combined with ECHO_BENCH_UI_FIXTURE; live observations must use the normal channel lifecycle"
        ));
    }

    let raw_channel = raw_channel.to_string_lossy();
    let channel = normalize_twitch_channel(&raw_channel)?;
    let receipt_path = echo_core::env::os_value(LIVE_CHANNEL_RECEIPT_ENV)
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
    let observation_nonce = echo_core::env::value(NONCE_ENV)
        .ok_or_else(|| format!("{NONCE_ENV} is required for a live-channel benchmark"))?;

    Ok(Some(LiveChannelBenchmark {
        channel,
        receipt_path,
        receipt_parent: canonical_receipt_parent,
        observation_nonce,
        profile_root: canonical_profile_root,
    }))
}

fn normalize_twitch_channel(raw_channel: &str) -> Result<String, String> {
    let channel = echo_core::channel_identity::normalize_channel_key(raw_channel.trim());
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
    let live_privmsg_count = inbound_privmsg_count();
    let messages_observed = live_privmsg_count > 0;
    let outbound_user_action_attempts = super::live_outbound_user_action_attempts();
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
