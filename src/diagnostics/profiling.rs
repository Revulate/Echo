//! Profiling and logging instrumentation.
//!
//! ## Features
//!
//! ### Chrome Tracing (`--features profiling`)
//! Generates Chrome DevTools-compatible trace files for `chrome://tracing` or `ui.perfetto.dev`.
//!
//! ```bash
//! cargo run --release --features profiling
//! # Close app normally, open echo_trace.json in chrome://tracing
//! ```
//!
//! ### Console Logging (default)
//! Colored console output controlled by `RUST_LOG` environment variable.
//!
//! ```bash
//! RUST_LOG=debug cargo run                    # Debug level
//! RUST_LOG=warn,echo_gpui=info cargo run      # Selective logging
//! RUST_LOG=trace cargo run                    # Everything
//! ```
//!
//! ### Release Default
//! A release build started with no `RUST_LOG`/`ECHO_LOG*`/trace env vars has no
//! console, so it writes warn and error events to `echo.log` under
//! [`crate::storage_roots::log_root`]. That file rotates to `echo.log.1` at
//! 5 MB and starts each session with a one-line header (version, target, OS
//! build, benchmark-profile state) whatever the level.
//!
//! ### File Logging
//! Set `ECHO_LOG_FILE=1` to also write logs to `~/.local/share/echo/echo.log`.
//! Set `ECHO_LOG_FILE_PATH=/path/to/echo.log` to choose a file.
//! Runtime commands can also enable or disable the file writer without restarting.
//! These opt-in debug files rotate at 50 MB.
//!
//! ```bash
//! ECHO_LOG_FILE=1 RUST_LOG=debug cargo run
//! ```

use tracing_subscriber::prelude::*;

#[cfg(not(feature = "profiling"))]
use std::{
    fs::{File, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
    sync::{
        Mutex, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
};

#[cfg(not(feature = "profiling"))]
use tracing_subscriber::{EnvFilter, Registry, fmt, reload};

#[cfg(feature = "profiling")]
use crate::backend::lock_utils::MutexExt;

#[cfg(feature = "profiling")]
use tracing_chrome::FlushGuard;

/// Guard that flushes trace on drop (must be kept alive for duration of app)
#[cfg(feature = "profiling")]
use std::sync::Mutex;

#[cfg(feature = "profiling")]
static TRACE_GUARD: Mutex<Option<TraceGuardHolder>> = Mutex::new(None);

#[cfg(feature = "profiling")]
struct TraceGuardHolder(FlushGuard);

#[cfg(not(feature = "profiling"))]
type RuntimeFilterHandle = reload::Handle<EnvFilter, Registry>;

#[cfg(not(feature = "profiling"))]
static RUNTIME_FILTER_HANDLE: OnceLock<RuntimeFilterHandle> = OnceLock::new();

#[cfg(not(feature = "profiling"))]
static RUNTIME_FILE_LOGGER: Mutex<Option<RuntimeLogFile>> = Mutex::new(None);

#[cfg(not(feature = "profiling"))]
static RUNTIME_CONSOLE_ACTIVE: AtomicBool = AtomicBool::new(false);

/// This release session started on the bounded `echo.log`, so that file is the
/// fallback whenever no opt-in debug file is installed.
#[cfg(not(feature = "profiling"))]
static RELEASE_DEFAULT_LOG: AtomicBool = AtomicBool::new(false);

#[cfg(not(feature = "profiling"))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LogFileKind {
    /// The always-on warn-level release log.
    ReleaseDefault,
    /// An opt-in debug log from `ECHO_LOG_FILE` or `/debug-enable-logfile`.
    Debug,
}

#[cfg(not(feature = "profiling"))]
impl LogFileKind {
    /// The file rotates to `<name>.1` at this size, bounding it to about twice this.
    fn max_bytes(self) -> u64 {
        match self {
            Self::ReleaseDefault => 5 * 1024 * 1024,
            Self::Debug => 50 * 1024 * 1024,
        }
    }
}

#[cfg(not(feature = "profiling"))]
struct RuntimeLogFile {
    path: PathBuf,
    file: File,
    written: u64,
    kind: LogFileKind,
}

#[cfg(not(feature = "profiling"))]
impl RuntimeLogFile {
    fn open(path: PathBuf, kind: LogFileKind) -> Result<Self, String> {
        let file = open_log_file(&path)?;
        let written = file.metadata().map(|metadata| metadata.len()).unwrap_or(0);
        Ok(Self {
            path,
            file,
            written,
            kind,
        })
    }

    fn record_written(&mut self, bytes: usize) {
        self.written = self.written.saturating_add(bytes as u64);
        if self.written >= self.kind.max_bytes() {
            self.rotate();
        }
    }

    fn rotate(&mut self) {
        let _ = self.file.flush();
        let mut rotated = self.path.clone().into_os_string();
        rotated.push(".1");
        let rotated = PathBuf::from(rotated);
        let _ = std::fs::remove_file(&rotated);
        if std::fs::rename(&self.path, &rotated).is_ok() {
            if let Ok(file) = open_log_file(&self.path) {
                self.file = file;
            }
        } else if self.file.set_len(0).is_err() {
            return;
        }
        self.written = 0;
    }
}

#[cfg(not(feature = "profiling"))]
#[derive(Clone, Copy, Debug)]
struct RuntimeFileMakeWriter;

#[cfg(not(feature = "profiling"))]
struct RuntimeFileWriter<'a> {
    guard: Option<std::sync::MutexGuard<'a, Option<RuntimeLogFile>>>,
}

#[cfg(not(feature = "profiling"))]
thread_local! {
    /// This thread holds the log file lock, so a nested event (the panic hook
    /// logging a panic raised mid-write) is dropped instead of deadlocking.
    static RUNTIME_LOG_WRITER_HELD: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Marks this thread as holding the log file lock until dropped, restoring
/// the previous mark.
#[cfg(not(feature = "profiling"))]
struct RuntimeLogWriterHeld {
    was_held: bool,
}

#[cfg(not(feature = "profiling"))]
impl RuntimeLogWriterHeld {
    fn mark() -> Self {
        Self {
            was_held: RUNTIME_LOG_WRITER_HELD.with(|held| held.replace(true)),
        }
    }
}

#[cfg(not(feature = "profiling"))]
impl Drop for RuntimeLogWriterHeld {
    fn drop(&mut self) {
        RUNTIME_LOG_WRITER_HELD.with(|held| held.set(self.was_held));
    }
}

#[cfg(not(feature = "profiling"))]
impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for RuntimeFileMakeWriter {
    type Writer = RuntimeFileWriter<'a>;

    fn make_writer(&'a self) -> Self::Writer {
        if RUNTIME_LOG_WRITER_HELD.with(std::cell::Cell::get) {
            return RuntimeFileWriter { guard: None };
        }
        let guard = RUNTIME_FILE_LOGGER.lock().ok();
        if guard.is_some() {
            RUNTIME_LOG_WRITER_HELD.with(|held| held.set(true));
        }
        RuntimeFileWriter { guard }
    }
}

#[cfg(not(feature = "profiling"))]
impl Drop for RuntimeFileWriter<'_> {
    fn drop(&mut self) {
        if self.guard.is_some() {
            RUNTIME_LOG_WRITER_HELD.with(|held| held.set(false));
        }
    }
}

#[cfg(not(feature = "profiling"))]
impl Write for RuntimeFileWriter<'_> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if let Some(guard) = self.guard.as_mut()
            && let Some(logger) = guard.as_mut()
        {
            let written = logger.file.write(buf)?;
            logger.record_written(written);
            return Ok(written);
        }

        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        if let Some(guard) = self.guard.as_mut()
            && let Some(logger) = guard.as_mut()
        {
            return logger.file.flush();
        }

        Ok(())
    }
}

/// Get the trace file path
#[cfg(feature = "profiling")]
fn get_trace_path() -> std::path::PathBuf {
    crate::storage_roots::profiling_artifact_path("echo_trace.json")
}

/// Get the log file path for file-based logging
#[cfg(not(feature = "profiling"))]
fn get_log_file_path() -> Option<PathBuf> {
    if echo_core::env::flag("ECHO_LOG_FILE") {
        if !crate::storage_roots::is_isolated()
            && let Some(path) = std::env::var_os("ECHO_LOG_FILE_PATH")
                .filter(|path| !path.is_empty())
                .map(std::path::PathBuf::from)
        {
            if let Some(parent) = path
                .parent()
                .filter(|parent| !parent.as_os_str().is_empty())
            {
                std::fs::create_dir_all(parent).ok()?;
            }
            return Some(path);
        }

        if let Some(log_dir) = crate::storage_roots::log_root() {
            std::fs::create_dir_all(&log_dir).ok()?;
            return Some(log_dir.join("echo.log"));
        }
    }
    None
}

#[cfg(not(feature = "profiling"))]
fn release_default_log_path() -> Option<PathBuf> {
    crate::storage_roots::log_root().map(|log_dir| log_dir.join("echo.log"))
}

/// One line identifying the session. It holds no user or account data.
#[cfg(not(feature = "profiling"))]
fn session_header() -> String {
    let os = sysinfo::System::long_os_version().unwrap_or_else(|_| "unknown OS".to_string());
    let build = sysinfo::System::kernel_version().unwrap_or_else(|_| "unknown".to_string());
    let profile = if crate::storage_roots::is_isolated() {
        "active"
    } else {
        "inactive"
    };
    format!(
        "==== session start {} | {} | OS {os} (build {build}) | benchmark profile {profile} | pid {} ====",
        chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        crate::identity::VERSION_REPORT.replace('\n', ", "),
        std::process::id(),
    )
}

/// Append one session-header detail line to the log file, whatever the log
/// level. The line must hold no user or account data.
#[cfg(not(feature = "profiling"))]
pub fn write_session_detail(line: &str) {
    write_runtime_log_line(line);
}

#[cfg(feature = "profiling")]
pub fn write_session_detail(line: &str) {
    tracing::info!("{line}");
}

#[cfg(not(feature = "profiling"))]
fn runtime_log_file_kind() -> Option<LogFileKind> {
    RUNTIME_FILE_LOGGER
        .lock()
        .ok()
        .and_then(|guard| guard.as_ref().map(|logger| logger.kind))
}

#[cfg(not(feature = "profiling"))]
fn runtime_console_active() -> bool {
    RUNTIME_CONSOLE_ACTIVE.load(Ordering::Relaxed)
}

#[cfg(not(feature = "profiling"))]
fn add_echo_category_filters(mut filter: EnvFilter, active_categories: &[&str]) -> EnvFilter {
    for category in crate::log::CATEGORY_TABLE {
        if active_categories.contains(&category.name)
            && let Ok(directive) = format!("{}=debug", category.target).parse()
        {
            filter = filter.add_directive(directive);
        }
    }

    filter
}

#[cfg(not(feature = "profiling"))]
fn add_gpui_noise_suppression(mut filter: EnvFilter) -> EnvFilter {
    for directive_str in ["gpui::window=off", "gpui::platform::windows::window=warn"] {
        if let Ok(directive) = directive_str.parse() {
            filter = filter.add_directive(directive);
        }
    }

    filter
}

#[cfg(not(feature = "profiling"))]
fn release_default_filter() -> EnvFilter {
    add_gpui_noise_suppression(EnvFilter::new("warn"))
}

#[cfg(not(feature = "profiling"))]
fn startup_filter(quiet_release: bool) -> EnvFilter {
    if quiet_release {
        return release_default_filter();
    }

    let mut filter = match EnvFilter::try_from_default_env() {
        Ok(filter) => filter,
        Err(_) => EnvFilter::new("warn"),
    };

    if std::env::var("RUST_LOG").is_err() {
        filter = add_echo_category_filters(filter, &crate::log::enabled_categories());
    }

    add_gpui_noise_suppression(filter)
}

#[cfg(not(feature = "profiling"))]
fn runtime_filter() -> EnvFilter {
    let file_kind = runtime_log_file_kind();
    let file_logging_enabled = file_kind == Some(LogFileKind::Debug);
    if !runtime_console_active() && !file_logging_enabled {
        if file_kind == Some(LogFileKind::ReleaseDefault) {
            return release_default_filter();
        }
        return EnvFilter::new("off");
    }

    let mut filter = match EnvFilter::try_from_default_env() {
        Ok(filter) => filter,
        Err(_) if file_logging_enabled => EnvFilter::new("warn,echo=debug"),
        Err(_) => EnvFilter::new("warn"),
    };

    let active_categories = crate::log::enabled_categories();
    filter = add_echo_category_filters(filter, &active_categories);

    add_gpui_noise_suppression(filter)
}

#[cfg(not(feature = "profiling"))]
fn open_log_file(path: &Path) -> Result<File, String> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("failed to create log directory: {error}"))?;
    }

    OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|error| format!("failed to open log file: {error}"))
}

#[cfg(not(feature = "profiling"))]
fn write_runtime_log_line(line: &str) {
    let held = RuntimeLogWriterHeld::mark();
    if held.was_held {
        return;
    }
    if let Ok(mut guard) = RUNTIME_FILE_LOGGER.lock()
        && let Some(logger) = guard.as_mut()
    {
        if writeln!(logger.file, "{line}").is_ok() {
            logger.record_written(line.len() + 1);
        }
        let _ = logger.file.flush();
    }
}

#[cfg(not(feature = "profiling"))]
fn set_runtime_log_file(
    path: PathBuf,
    kind: LogFileKind,
    announce: bool,
) -> Result<PathBuf, String> {
    let logger = RuntimeLogFile::open(path.clone(), kind)?;
    {
        let mut guard = RUNTIME_FILE_LOGGER
            .lock()
            .map_err(|_| "runtime file logger lock was poisoned".to_string())?;
        *guard = Some(logger);
    }

    refresh_runtime_filter()?;
    if announce {
        write_runtime_log_line(&format!(
            "Echo runtime file logging enabled: {}",
            path.display()
        ));
    }

    Ok(path)
}

#[cfg(not(feature = "profiling"))]
pub fn enable_runtime_log_file(path: impl AsRef<Path>) -> Result<PathBuf, String> {
    let path = if crate::storage_roots::is_isolated() {
        crate::storage_roots::log_root()
            .ok_or_else(|| "benchmark profile log root is unavailable".to_string())?
            .join("echo.runtime.log")
    } else {
        path.as_ref().to_path_buf()
    };
    set_runtime_log_file(path, LogFileKind::Debug, true)
}

#[cfg(feature = "profiling")]
pub fn enable_runtime_log_file(
    _path: impl AsRef<std::path::Path>,
) -> Result<std::path::PathBuf, String> {
    Err("runtime file logging is unavailable while Chrome profiling is enabled".to_string())
}

#[cfg(not(feature = "profiling"))]
pub fn disable_runtime_log_file() -> Result<Option<PathBuf>, String> {
    let path = {
        let _held = RuntimeLogWriterHeld::mark();
        let mut guard = RUNTIME_FILE_LOGGER
            .lock()
            .map_err(|_| "runtime file logger lock was poisoned".to_string())?;
        if guard
            .as_ref()
            .is_some_and(|logger| logger.kind == LogFileKind::ReleaseDefault)
        {
            return Ok(None);
        }
        if let Some(logger) = guard.as_mut() {
            let _ = writeln!(
                logger.file,
                "Echo runtime file logging disabled: {}",
                logger.path.display()
            );
            let _ = logger.file.flush();
        }
        guard.take().map(|logger| logger.path)
    };

    if RELEASE_DEFAULT_LOG.load(Ordering::Relaxed)
        && let Some(default_path) = release_default_log_path()
    {
        let _ = set_runtime_log_file(default_path, LogFileKind::ReleaseDefault, false);
    }
    refresh_runtime_filter()?;
    Ok(path)
}

#[cfg(feature = "profiling")]
pub fn disable_runtime_log_file() -> Result<Option<std::path::PathBuf>, String> {
    Err("runtime file logging is unavailable while Chrome profiling is enabled".to_string())
}

#[cfg(not(feature = "profiling"))]
fn refresh_runtime_filter() -> Result<(), String> {
    if let Some(handle) = RUNTIME_FILTER_HANDLE.get() {
        handle
            .reload(runtime_filter())
            .map_err(|error| format!("failed to reload logging filter: {error}"))?;
    }
    Ok(())
}

/// Initialize the tracing/profiling system.
///
/// ## Modes
///
/// 1. **Profiling mode** (`--features profiling`):
///    - Chrome tracing layer for flame graphs
///    - Outputs to `echo_trace.json`
///
/// 2. **Normal mode** (default):
///    - Colored console output with timestamps
///    - Optional file logging with `ECHO_LOG_FILE=1`
///    - Log level controlled by `RUST_LOG` (default: warn)
///    - A release build with no logging env vars writes warn+ to a 5 MB
///      rotating `echo.log` instead of the console it lacks; with logging env
///      vars but no `ECHO_LOG_FILE` it writes the env-derived level there
pub fn init() {
    #[cfg(feature = "profiling")]
    {
        use tracing_chrome::ChromeLayerBuilder;

        let trace_path = get_trace_path();
        let trace_path_str = trace_path.display().to_string();

        let (chrome_layer, guard) = ChromeLayerBuilder::new()
            .file(&trace_path)
            .include_args(true)
            .include_locations(true)
            .build();

        tracing_subscriber::registry()
            .with(chrome_layer)
            .with(
                tracing_subscriber::EnvFilter::builder()
                    .with_default_directive(tracing::Level::TRACE.into())
                    .from_env_lossy(),
            )
            .init();

        *TRACE_GUARD.lock_or_recover() = Some(TraceGuardHolder(guard));

        eprintln!("\x1b[35m╔══════════════════════════════════════╗\x1b[0m");
        eprintln!(
            "\x1b[35m║\x1b[0m  \x1b[1;33mPROFILING ENABLED\x1b[0m                   \x1b[35m║\x1b[0m"
        );
        eprintln!("\x1b[35m╠══════════════════════════════════════╣\x1b[0m");
        eprintln!(
            "\x1b[35m║\x1b[0m  Trace: \x1b[36m{:<27}\x1b[0m \x1b[35m║\x1b[0m",
            trace_path_str
        );
        eprintln!("\x1b[35m║\x1b[0m  Close app normally to flush.        \x1b[35m║\x1b[0m");
        eprintln!("\x1b[35m╚══════════════════════════════════════╝\x1b[0m");

        tracing::info!(path = %trace_path_str, "Profiling enabled");
    }

    #[cfg(not(feature = "profiling"))]
    {
        let quiet_release = {
            #[cfg(not(debug_assertions))]
            {
                let has_rust_log = std::env::var("RUST_LOG").is_ok();
                let has_echo_log =
                    echo_core::env::flag("ECHO_LOG_FILE") || crate::log::any_category_switch_set();

                !has_rust_log && !has_echo_log
            }
            #[cfg(debug_assertions)]
            {
                false
            }
        };

        RUNTIME_CONSOLE_ACTIVE.store(!quiet_release, Ordering::Relaxed);

        let filter = startup_filter(quiet_release);
        let (filter_layer, filter_handle) = reload::Layer::new(filter);
        let _ = RUNTIME_FILTER_HANDLE.set(filter_handle);

        let subscriber = tracing_subscriber::registry().with(filter_layer);

        let console_layer = fmt::layer()
            .with_ansi(true)
            .with_target(true)
            .with_thread_ids(false)
            .with_file(false)
            .with_line_number(false)
            .compact();

        let (startup_log_path, startup_log_kind) = if quiet_release {
            (release_default_log_path(), LogFileKind::ReleaseDefault)
        } else {
            match get_log_file_path() {
                Some(path) => (Some(path), LogFileKind::Debug),
                // A release build has no console, so a logging env var without
                // ECHO_LOG_FILE still records into the bounded default log.
                None if cfg!(not(debug_assertions)) => {
                    (release_default_log_path(), LogFileKind::ReleaseDefault)
                }
                None => (None, LogFileKind::Debug),
            }
        };
        RELEASE_DEFAULT_LOG.store(
            startup_log_kind == LogFileKind::ReleaseDefault,
            Ordering::Relaxed,
        );
        let startup_log_result = startup_log_path
            .as_ref()
            .map(|path| set_runtime_log_file(path.clone(), startup_log_kind, false));

        if quiet_release {
            let file_layer = fmt::layer()
                .with_ansi(false)
                .with_target(true)
                .with_file(true)
                .with_line_number(true)
                .with_writer(RuntimeFileMakeWriter);
            subscriber.with(file_layer).init();
        } else {
            let file_layer = fmt::layer()
                .with_ansi(false)
                .with_target(true)
                .with_file(true)
                .with_line_number(true)
                .with_writer(RuntimeFileMakeWriter);
            subscriber.with(console_layer).with(file_layer).init();
        }

        if let Some(result) = startup_log_result {
            match result {
                Ok(_) if startup_log_kind == LogFileKind::ReleaseDefault => {
                    write_runtime_log_line(&session_header());
                }
                Ok(log_path) => {
                    write_runtime_log_line(&session_header());
                    eprintln!(
                        "\x1b[32m[LOG]\x1b[0m File logging enabled: {}",
                        log_path.display()
                    );
                    write_runtime_log_line(&format!(
                        "Echo startup file logging enabled: {}",
                        log_path.display()
                    ));
                }
                Err(e) => {
                    eprintln!("\x1b[33m[WARN]\x1b[0m Could not open log file: {}", e);
                }
            }
        }

        #[cfg(debug_assertions)]
        {
            tracing::info!("Echo GPUI starting (debug mode)");
            tracing::debug!(
                "Log level: {}",
                std::env::var("RUST_LOG").unwrap_or_else(|_| "warn (default)".into())
            );
        }
    }
}
