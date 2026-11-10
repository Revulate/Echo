//! Startup reliability helpers for crash recovery, safe mode, and degraded-service surfacing.

use super::lock_utils::MutexExt;
use std::collections::hash_map::Entry;
use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
use std::fs;
use std::panic::PanicHookInfo;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{LazyLock, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const SAFE_MODE_FLAG: &str = "--safe-mode";
const RUN_MARKER_FILE: &str = "runtime-active.marker";
const LAST_PANIC_FILE: &str = "last-panic.txt";
const CRASH_DIR: &str = "crashes";
const MAX_CRASH_REPORTS: usize = 10;

static SAFE_MODE: AtomicBool = AtomicBool::new(false);
static PANIC_HOOK_INSTALLED: AtomicBool = AtomicBool::new(false);
static GUI_RUN_ACTIVE: AtomicBool = AtomicBool::new(false);
/// Set once `storage_roots` has chosen the storage roots; a panic before
/// that writes no files, so an isolated profile never leaks into normal state.
static STORAGE_ROOTS_READY: AtomicBool = AtomicBool::new(false);
static PROCESS_STARTED: OnceLock<Instant> = OnceLock::new();
static STARTUP_NOTICES: LazyLock<Mutex<Vec<ReliabilityNotice>>> =
    LazyLock::new(|| Mutex::new(Vec::new()));
static STORAGE_RECOVERY_NOTICES: LazyLock<Mutex<HashSet<&'static str>>> =
    LazyLock::new(|| Mutex::new(HashSet::new()));
static DEGRADED_PROVIDERS: LazyLock<Mutex<HashMap<&'static str, String>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReliabilityNoticeLevel {
    Info,
    Warning,
}

#[derive(Clone, Debug)]
pub struct ReliabilityNotice {
    pub level: ReliabilityNoticeLevel,
    pub message: String,
    pub detail: Option<String>,
}

#[derive(Clone, Copy, Debug)]
pub enum Provider {
    EventSub,
    SevenTvEventApi,
}

impl Provider {
    fn key(self) -> &'static str {
        match self {
            Self::EventSub => "eventsub",
            Self::SevenTvEventApi => "7tv-eventapi",
        }
    }

    fn degraded_message(self) -> &'static str {
        match self {
            Self::EventSub => "Twitch EventSub is degraded",
            Self::SevenTvEventApi => "7TV real-time updates are degraded",
        }
    }

    fn restored_message(self) -> &'static str {
        match self {
            Self::EventSub => "Twitch EventSub recovered",
            Self::SevenTvEventApi => "7TV real-time updates recovered",
        }
    }
}

pub fn initialize_for_current_run(args: &[String]) {
    SAFE_MODE.store(
        args.iter()
            .any(|arg| arg.eq_ignore_ascii_case(SAFE_MODE_FLAG)),
        Ordering::Release,
    );

    install_panic_hook();
    GUI_RUN_ACTIVE.store(true, Ordering::Release);

    let (previous_unclean_exit, previous_panic_summary) = match stale_run_marker_pid() {
        StaleMarker::OwnerStillRunning => {
            crate::log_ui!(
                debug,
                "Run marker belongs to a live Echo instance; skipping unclean-exit notice"
            );
            (false, None)
        }
        marker => (
            matches!(marker, StaleMarker::Stale),
            take_last_panic_summary(),
        ),
    };

    if previous_unclean_exit {
        let detail = match (previous_panic_summary, is_safe_mode()) {
            (Some(summary), true) => Some(format!("Crash: {summary}")),
            (Some(summary), false) => Some(format!(
                "Crash: {summary}\nStart with {SAFE_MODE_FLAG} if this keeps happening."
            )),
            (None, true) => None,
            (None, false) => Some(format!(
                "If this keeps happening, start Echo with {SAFE_MODE_FLAG}."
            )),
        };
        push_startup_notice(
            ReliabilityNoticeLevel::Warning,
            "Previous session ended unexpectedly",
            detail,
        );
    } else if let Some(summary) = previous_panic_summary {
        push_startup_notice(
            ReliabilityNoticeLevel::Info,
            "Previous session recorded an internal error",
            Some(format!("Error: {summary}")),
        );
    }

    if is_safe_mode() {
        push_startup_notice(
            ReliabilityNoticeLevel::Info,
            "Safe mode enabled",
            Some("EventSub and 7TV real-time services are disabled for this run.".to_string()),
        );
    }

    write_run_marker();
}

/// Called by `main` once the benchmark profile has fixed the storage roots.
pub fn note_storage_roots_ready() {
    STORAGE_ROOTS_READY.store(true, Ordering::Release);
}

pub fn is_safe_mode() -> bool {
    SAFE_MODE.load(Ordering::Acquire)
}

pub fn mark_clean_shutdown() {
    if let Some(path) = run_marker_path() {
        let _ = fs::remove_file(path);
    }
}

/// Folder holding the newest crash reports.
pub fn crash_reports_dir() -> Option<PathBuf> {
    reliability_dir().map(|dir| dir.join(CRASH_DIR))
}

pub fn note_startup_degraded_feature(message: impl Into<String>, detail: impl Into<String>) {
    push_startup_notice(
        ReliabilityNoticeLevel::Warning,
        message,
        Some(detail.into()),
    );
}

pub(crate) fn note_storage_recovery(
    what: &'static str,
    primary: &Path,
    recovered_from: Option<&Path>,
    preserved: &[PathBuf],
    unpreserved: &[PathBuf],
) {
    if !STORAGE_RECOVERY_NOTICES.lock_or_recover().insert(what) {
        return;
    }
    let (message, mut detail) = match recovered_from {
        Some(backup) => (
            format!("{what} recovered from backup"),
            format!(
                "Loaded {} instead of {}.",
                backup.display(),
                primary.display()
            ),
        ),
        None => (
            format!("{what} could not be recovered; using defaults"),
            format!(
                "No usable primary or backup was found for {}.",
                primary.display()
            ),
        ),
    };
    for path in preserved {
        let _ = write!(detail, "\nPreserved unreadable copy: {}", path.display());
    }
    for path in unpreserved {
        let _ = write!(
            detail,
            "\nCould not copy aside: {}. The original is protected from overwrite.",
            path.display()
        );
    }
    note_startup_degraded_feature(message, detail);
}

pub fn drain_startup_notices() -> Vec<ReliabilityNotice> {
    std::mem::take(&mut *STARTUP_NOTICES.lock_or_recover())
}

pub fn notify_provider_degraded(provider: Provider, detail: impl Into<String>) {
    let detail = detail.into();
    let should_notify = {
        let mut degraded = DEGRADED_PROVIDERS.lock_or_recover();
        match degraded.entry(provider.key()) {
            Entry::Occupied(_) => false,
            Entry::Vacant(entry) => {
                entry.insert(detail.clone());
                true
            }
        }
    };

    if should_notify {
        crate::backend::push_ui_action(crate::backend::UiAction::ShowWarningToast {
            message: provider.degraded_message().to_string(),
            detail: Some(detail),
        });
    }
}

pub fn notify_provider_recovered(provider: Provider) {
    let should_notify = DEGRADED_PROVIDERS
        .lock_or_recover()
        .remove(provider.key())
        .is_some();

    if should_notify {
        crate::backend::push_ui_action(crate::backend::UiAction::ShowSuccessToast {
            message: provider.restored_message().to_string(),
            detail: None,
        });
    }
}

fn push_startup_notice(
    level: ReliabilityNoticeLevel,
    message: impl Into<String>,
    detail: Option<String>,
) {
    STARTUP_NOTICES.lock_or_recover().push(ReliabilityNotice {
        level,
        message: message.into(),
        detail,
    });
}

/// Install the crash-report panic hook. Idempotent; `main` calls this first.
pub fn install_panic_hook() {
    PROCESS_STARTED.get_or_init(Instant::now);
    if PANIC_HOOK_INSTALLED
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return;
    }

    let previous_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |panic_info| {
        persist_panic_summary(panic_info);
        previous_hook(panic_info);
    }));
}

fn persist_panic_summary(panic_info: &PanicHookInfo<'_>) {
    let payload = panic_info
        .payload_as_str()
        .unwrap_or("panic payload unavailable");
    let location = panic_info
        .location()
        .map(|location| format!(" at {}:{}", location.file(), location.line()))
        .unwrap_or_default();
    let summary = format!("{payload}{location}");

    if !STORAGE_ROOTS_READY.load(Ordering::Acquire) {
        return;
    }
    if GUI_RUN_ACTIVE.load(Ordering::Acquire)
        && let Some(path) = last_panic_path()
    {
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        if let Err(error) = fs::write(&path, format!("{summary}\n")) {
            crate::log_app!(
                warn,
                "Could not write the last-panic marker {}: {error}",
                path.display()
            );
        }
    }

    if let Some(dir) = crash_reports_dir() {
        write_crash_report(&dir, &summary);
    }
    tracing::error!(target: "echo::crash", "panic: {summary}");
}

fn write_crash_report(dir: &Path, summary: &str) {
    if fs::create_dir_all(dir).is_err() {
        return;
    }
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let pid = std::process::id();
    let thread = std::thread::current();
    let session = PROCESS_STARTED
        .get()
        .map(Instant::elapsed)
        .unwrap_or_default();

    let mut report = String::with_capacity(8 * 1024);
    let _ = writeln!(report, "Echo crash report");
    let _ = writeln!(
        report,
        "version: {} ({}, gpui {})",
        env!("CARGO_PKG_VERSION"),
        env!("ECHO_BUILD_TARGET"),
        env!("ECHO_GPUI_ZED_REV")
    );
    let _ = writeln!(
        report,
        "time: {} (unix {})",
        chrono::Utc::now().to_rfc3339(),
        now.as_secs()
    );
    let _ = writeln!(report, "pid: {pid}");
    let _ = writeln!(
        report,
        "thread: {} ({:?})",
        thread.name().unwrap_or("<unnamed>"),
        thread.id()
    );
    let _ = writeln!(report, "session: {:.1}s", session.as_secs_f64());
    if let Some(uptime) = monotonic_clock_uptime() {
        let _ = writeln!(report, "system uptime: {}s", uptime.as_secs());
    }
    if let Some(base) = image_base() {
        let _ = writeln!(report, "image base: {base:#x}");
    }
    let _ = writeln!(report, "panic: {summary}");
    let _ = writeln!(report);
    let _ = writeln!(
        report,
        "backtrace:\n{:#}",
        std::backtrace::Backtrace::force_capture()
    );

    let path = dir.join(format!("crash-{:013}-{pid}.txt", now.as_millis()));
    if fs::write(path, report).is_ok() {
        prune_crash_reports(dir);
    }
}

fn prune_crash_reports(dir: &Path) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let mut reports: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("crash-") && name.ends_with(".txt"))
        })
        .collect();
    if reports.len() <= MAX_CRASH_REPORTS {
        return;
    }
    reports.sort();
    for path in &reports[..reports.len() - MAX_CRASH_REPORTS] {
        let _ = fs::remove_file(path);
    }
}

#[cfg(windows)]
fn monotonic_clock_uptime() -> Option<Duration> {
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetTickCount64() -> u64;
    }
    // SAFETY: GetTickCount64 takes no arguments and has no preconditions.
    Some(Duration::from_millis(unsafe { GetTickCount64() }))
}

#[cfg(not(windows))]
fn monotonic_clock_uptime() -> Option<Duration> {
    None
}

#[cfg(all(windows, target_env = "msvc"))]
fn image_base() -> Option<usize> {
    unsafe extern "C" {
        static __ImageBase: u8;
    }
    Some(&raw const __ImageBase as usize)
}

#[cfg(not(all(windows, target_env = "msvc")))]
fn image_base() -> Option<usize> {
    None
}

fn write_run_marker() {
    let Some(path) = run_marker_path() else {
        return;
    };

    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }

    if let Err(error) = fs::write(&path, format!("pid={}\n", std::process::id())) {
        crate::log_app!(
            warn,
            "Could not write the run marker {}; the next launch cannot detect an unclean exit: {error}",
            path.display()
        );
    }
}

/// Outcome of inspecting the leftover run marker from a previous launch.
enum StaleMarker {
    /// No marker on disk — previous run exited cleanly (or first run).
    None,
    /// Marker exists and its owner is gone — genuine unclean exit.
    Stale,
    /// Marker exists but its pid is a live Echo process — concurrent instance.
    OwnerStillRunning,
}

fn stale_run_marker_pid() -> StaleMarker {
    let Some(path) = run_marker_path() else {
        return StaleMarker::None;
    };
    let Ok(content) = fs::read_to_string(&path) else {
        return if path.exists() {
            StaleMarker::Stale
        } else {
            StaleMarker::None
        };
    };

    let marker_pid = content
        .trim()
        .strip_prefix("pid=")
        .and_then(|pid| pid.parse::<u32>().ok());
    match marker_pid {
        Some(pid) if pid != std::process::id() && is_live_echo_process(pid) => {
            StaleMarker::OwnerStillRunning
        }
        _ => StaleMarker::Stale,
    }
}

/// Check whether `pid` is a currently running process whose executable looks
/// like an Echo binary (same file stem as the current exe, fallback "echo").
fn is_live_echo_process(pid: u32) -> bool {
    use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};

    let Ok(mut sys) = System::new() else {
        return false;
    };
    let target = Pid::from_u32(pid);
    sys.refresh_processes_specifics(
        ProcessesToUpdate::Some(&[target]),
        true,
        ProcessRefreshKind::nothing(),
    );
    let Some(process) = sys.process(target) else {
        return false;
    };

    let own_stem = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.file_stem().map(|s| s.to_string_lossy().to_lowercase()));
    let own_stem = own_stem.as_deref().unwrap_or("echo");
    process
        .name()
        .to_string_lossy()
        .to_lowercase()
        .starts_with(own_stem)
}

/// Maximum characters of a stored panic message surfaced in the startup toast.
/// The full text is in the crash report; the toast only needs the gist.
const PANIC_SUMMARY_MAX_CHARS: usize = 140;

fn take_last_panic_summary() -> Option<String> {
    let path = last_panic_path()?;
    let content = fs::read_to_string(&path).ok()?;
    let _ = fs::remove_file(&path);
    let trimmed = content.trim();
    if trimmed.is_empty() {
        return None;
    }
    let mut summary: String = trimmed.chars().take(PANIC_SUMMARY_MAX_CHARS).collect();
    if summary.len() < trimmed.len() {
        summary.push('…');
    }
    Some(summary)
}

fn run_marker_path() -> Option<PathBuf> {
    reliability_dir().map(|dir| dir.join(RUN_MARKER_FILE))
}

fn last_panic_path() -> Option<PathBuf> {
    reliability_dir().map(|dir| dir.join(LAST_PANIC_FILE))
}

fn reliability_dir() -> Option<PathBuf> {
    crate::storage_roots::config_root()
}
