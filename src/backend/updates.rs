//! Signed, bounded application-update client.
//!
//! The private release repository is a build/publishing origin. Installed Echo
//! clients fetch a credential-free envelope from the signed update origin and verify its exact
//! payload bytes with the embedded Ed25519 public key before trusting version,
//! changelog, package URL, size, or SHA-256 metadata.

use reqwest::StatusCode;
use ring::digest;
use serde::Deserialize;
#[cfg(target_os = "windows")]
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
#[cfg(target_os = "windows")]
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;
use tokio::io::AsyncWriteExt;

const UPDATE_ORIGIN: &str = "https://echo-updates.revulate.dev";
const UPDATE_HOST: &str = "echo-updates.revulate.dev";
const UPDATE_PATH_PREFIX: &str = "/stable/";
const MAX_MANIFEST_BYTES: usize = echo_update_core::MAX_MANIFEST_BYTES;
const MAX_CHANGELOG_BYTES: usize = 16 * 1024;
const MAX_PACKAGE_BYTES: u64 = echo_update_core::MAX_PACKAGE_BYTES;
const MANIFEST_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const MANIFEST_REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
const PACKAGE_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const PACKAGE_REQUEST_TIMEOUT: Duration = Duration::from_secs(120);
#[cfg(target_os = "windows")]
const STAGED_INSTALLER_NAME: &str = echo_update_core::STAGED_HELPER_NAME;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum UpdateStateKind {
    #[default]
    Idle,
    Checking,
    UpToDate,
    Available,
    Downloading,
    Ready,
    Failed,
}

#[derive(Clone, Debug)]
pub struct UpdateSnapshot {
    pub kind: UpdateStateKind,
    pub current_version: String,
    pub latest_version: Option<String>,
    pub published_at: Option<String>,
    pub changelog: Option<String>,
    pub detail: Option<String>,
}

#[derive(Clone, Debug)]
struct VerifiedRelease {
    version: semver::Version,
    published_at: String,
    changelog: String,
    package: VerifiedPackage,
    /// The signed envelope exactly as fetched. The installer re-verifies it,
    /// so it never trusts package metadata passed on its command line.
    /// Only the Windows installer consumes it.
    #[cfg_attr(not(target_os = "windows"), allow(dead_code))]
    envelope: Arc<[u8]>,
}

#[derive(Clone, Debug)]
struct VerifiedPackage {
    url: String,
    bytes: u64,
    sha256: String,
}

#[derive(Default)]
struct UpdateState {
    kind: UpdateStateKind,
    release: Option<VerifiedRelease>,
    downloaded_path: Option<PathBuf>,
    detail: Option<String>,
}

static BENCHMARK_STATE: LazyLock<Mutex<Option<UpdateState>>> = LazyLock::new(|| Mutex::new(None));

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ManifestPayload {
    schema_version: u32,
    channel: String,
    version: String,
    published_at: String,
    changelog: String,
    package: ManifestPackage,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ManifestPackage {
    platform: String,
    url: String,
    bytes: u64,
    sha256: String,
}

static STATE: LazyLock<Mutex<UpdateState>> = LazyLock::new(|| Mutex::new(UpdateState::default()));
static CHECK_IN_FLIGHT: AtomicBool = AtomicBool::new(false);
static DOWNLOAD_IN_FLIGHT: AtomicBool = AtomicBool::new(false);
static AUTO_CHECK_STARTED: AtomicBool = AtomicBool::new(false);
/// Wall-clock time (Unix seconds) the last update check started, manual or
/// automatic. Wall clock rather than `Instant`, so time asleep counts.
static LAST_CHECK_UNIX_SECS: AtomicU64 = AtomicU64::new(0);
static STATE_CHANGE_GENERATION: AtomicU64 = AtomicU64::new(0);
static STATE_CHANGES: LazyLock<tokio::sync::watch::Sender<u64>> = LazyLock::new(|| {
    let (sender, _) = tokio::sync::watch::channel(0);
    sender
});

fn state() -> std::sync::MutexGuard<'static, UpdateState> {
    STATE.lock().unwrap_or_else(|error| error.into_inner())
}

fn bump_state_generation() -> u64 {
    STATE_CHANGE_GENERATION.fetch_add(1, Ordering::AcqRel) + 1
}

fn notify_state_changed() {
    let generation = bump_state_generation();
    STATE_CHANGES.send_replace(generation);
    crate::backend::push_ui_action(crate::backend::UiAction::UpdateStateChanged);
}

pub fn subscribe_state_changes() -> tokio::sync::watch::Receiver<u64> {
    STATE_CHANGES.subscribe()
}

/// Monotonic counter bumped whenever the state `snapshot` would report changes.
/// Renderers cache derived values against it instead of re-reading the state.
pub fn state_generation() -> u64 {
    STATE_CHANGE_GENERATION.load(Ordering::Acquire)
}

/// Lifecycle state alone, without cloning the release metadata strings.
pub fn state_kind() -> UpdateStateKind {
    let benchmark_state = BENCHMARK_STATE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    if let Some(state) = benchmark_state.as_ref() {
        return state.kind;
    }
    drop(benchmark_state);

    state().kind
}

pub fn snapshot() -> UpdateSnapshot {
    let benchmark_state = BENCHMARK_STATE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    if let Some(state) = benchmark_state.as_ref() {
        return snapshot_from_state(state);
    }
    drop(benchmark_state);

    let state = state();
    snapshot_from_state(&state)
}

fn snapshot_from_state(state: &UpdateState) -> UpdateSnapshot {
    UpdateSnapshot {
        kind: state.kind,
        current_version: env!("CARGO_PKG_VERSION").to_string(),
        latest_version: state
            .release
            .as_ref()
            .map(|release| release.version.to_string()),
        published_at: state
            .release
            .as_ref()
            .map(|release| release.published_at.clone()),
        changelog: state
            .release
            .as_ref()
            .map(|release| release.changelog.clone()),
        detail: state.detail.clone(),
    }
}

fn compiled_version() -> semver::Version {
    semver::Version::parse(env!("CARGO_PKG_VERSION"))
        .expect("the compiled Echo package version must be valid semver")
}

/// Present deterministic, non-persistent updater states to the existing live
/// UI acceptance harness. These snapshots never enter the download/install
/// state machine, so fixture metadata cannot be installed accidentally.
pub(crate) fn show_up_to_date_for_benchmark() {
    set_benchmark_release(UpdateStateKind::UpToDate, compiled_version());
}

pub(crate) fn show_available_for_benchmark() -> String {
    let mut version = compiled_version();
    version.patch = version.patch.saturating_add(1);
    version.pre = semver::Prerelease::EMPTY;
    version.build = semver::BuildMetadata::EMPTY;
    let rendered = version.to_string();
    set_benchmark_release(UpdateStateKind::Available, version);
    rendered
}

pub(crate) fn clear_benchmark_snapshot() {
    *BENCHMARK_STATE
        .lock()
        .unwrap_or_else(|error| error.into_inner()) = None;
    bump_state_generation();
}

fn set_benchmark_release(kind: UpdateStateKind, version: semver::Version) {
    let detail = match kind {
        UpdateStateKind::UpToDate => "You are running the latest stable Echo release",
        UpdateStateKind::Available => "A verified Echo update is ready to download",
        _ => unreachable!("benchmark release fixtures only cover stable UI states"),
    };
    let version_text = version.to_string();
    *BENCHMARK_STATE
        .lock()
        .unwrap_or_else(|error| error.into_inner()) = Some(UpdateState {
        kind,
        release: Some(VerifiedRelease {
            version,
            published_at: "2026-07-27T00:00:00Z".to_string(),
            changelog: format!(
                "Echo {version_text} update status and single-executable updater verification."
            ),
            package: VerifiedPackage {
                url: format!(
                    "{UPDATE_ORIGIN}/stable/{}/releases/{version_text}/echo.exe",
                    platform_id()
                ),
                bytes: 1,
                sha256: "0".repeat(64),
            },
            envelope: Arc::from([]),
        }),
        downloaded_path: None,
        detail: Some(detail.to_string()),
    });
    bump_state_generation();
}

/// Start one delayed startup check when the user has not disabled it.
/// Check shortly after startup, then keep re-checking while Echo runs.
///
/// Echo is often left running for days; with a check only at startup, a
/// running Echo would never learn about a release. The loop wakes on a short
/// tick and checks once [`echo_update_core::automatic_recheck_due`] says the
/// interval has passed in wall-clock time, so sleep and hibernation do not
/// stall it. It skips while automatic checks are off, and while a release is
/// already offered, downloading or staged.
pub fn start_automatic_check() {
    if !crate::backend::settings::get_auto_check_updates()
        || AUTO_CHECK_STARTED.swap(true, Ordering::AcqRel)
    {
        return;
    }
    crate::backend::bridge::RUNTIME.spawn(async {
        tokio::time::sleep(Duration::from_secs(5)).await;
        check_for_updates(false);

        let interval = recheck_interval();
        let tick = (interval / 4).clamp(Duration::from_secs(5), Duration::from_secs(15 * 60));
        loop {
            tokio::time::sleep(tick).await;
            if !crate::backend::settings::get_auto_check_updates() {
                continue;
            }
            if echo_update_core::automatic_recheck_due(
                recheck_state(),
                time_since_last_check(),
                interval,
            ) {
                check_for_updates(false);
            }
        }
    });
}

/// `ECHO_UPDATE_RECHECK_SECS` shortens the re-check interval for testing.
fn recheck_interval() -> Duration {
    std::env::var("ECHO_UPDATE_RECHECK_SECS")
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok())
        .filter(|&seconds| seconds > 0)
        .map_or(echo_update_core::RECHECK_INTERVAL, Duration::from_secs)
}

fn recheck_state() -> echo_update_core::RecheckState {
    use echo_update_core::RecheckState;
    match state_kind() {
        UpdateStateKind::Idle | UpdateStateKind::UpToDate => RecheckState::Settled,
        UpdateStateKind::Failed => RecheckState::Failed,
        UpdateStateKind::Checking
        | UpdateStateKind::Available
        | UpdateStateKind::Downloading
        | UpdateStateKind::Ready => RecheckState::Busy,
    }
}

fn unix_now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
}

fn time_since_last_check() -> Duration {
    Duration::from_secs(
        unix_now_secs().saturating_sub(LAST_CHECK_UNIX_SECS.load(Ordering::Acquire)),
    )
}

/// Check the signed stable update-origin manifest. Concurrent requests coalesce.
pub fn check_for_updates(manual: bool) {
    if CHECK_IN_FLIGHT.swap(true, Ordering::AcqRel) {
        return;
    }
    LAST_CHECK_UNIX_SECS.store(unix_now_secs(), Ordering::Release);
    {
        let mut state = state();
        state.kind = UpdateStateKind::Checking;
        state.release = None;
        state.downloaded_path = None;
        state.detail = Some("Fetching and verifying the signed manifest…".to_string());
    }
    notify_state_changed();

    crate::log_update!(
        info,
        manual,
        platform = platform_id(),
        "Checking for updates"
    );
    crate::backend::bridge::RUNTIME.spawn(async move {
        let result = fetch_verified_release().await;
        CHECK_IN_FLIGHT.store(false, Ordering::Release);
        match result {
            Ok(Some(release)) if release.version > compiled_version() => {
                let version = release.version.to_string();
                crate::log_update!(info, %version, "Update check found a newer release");
                {
                    let mut state = state();
                    state.kind = UpdateStateKind::Available;
                    state.release = Some(release);
                    state.downloaded_path = None;
                    state.detail = Some(format!("Echo {version} is ready to download"));
                }
                notify_state_changed();
                crate::backend::push_ui_action(
                    crate::backend::UiAction::ShowUpdateAvailableToast { version },
                );
            }
            Ok(release) => {
                crate::log_update!(
                    info,
                    latest = ?release.as_ref().map(|release| release.version.to_string()),
                    "Update check: Echo is up to date"
                );
                let mut state = state();
                state.kind = UpdateStateKind::UpToDate;
                state.release = release;
                state.downloaded_path = None;
                state.detail = Some(if state.release.is_some() {
                    "You are running the latest stable Echo release".to_string()
                } else {
                    "No newer stable Echo release is currently published".to_string()
                });
                drop(state);
                notify_state_changed();
                if manual {
                    crate::backend::push_ui_action(crate::backend::UiAction::ShowSuccessToast {
                        message: "Echo is up to date".to_string(),
                        detail: Some(format!("Current version {}", env!("CARGO_PKG_VERSION"))),
                    });
                }
            }
            Err(error) => {
                log_update_failure("Update check", &error);
                let mut state = state();
                state.kind = UpdateStateKind::Failed;
                state.release = None;
                state.downloaded_path = None;
                state.detail = Some(error.clone());
                drop(state);
                notify_state_changed();
                if manual {
                    crate::backend::push_ui_action(crate::backend::UiAction::ShowWarningToast {
                        message: "Update check failed".to_string(),
                        detail: Some(error),
                    });
                }
            }
        }
    });
}

/// Download and verify the currently advertised package.
pub fn download_available_update() {
    if DOWNLOAD_IN_FLIGHT.swap(true, Ordering::AcqRel) {
        return;
    }
    let release = {
        let mut state = state();
        let Some(release) = state.release.clone() else {
            DOWNLOAD_IN_FLIGHT.store(false, Ordering::Release);
            return;
        };
        state.kind = UpdateStateKind::Downloading;
        state.detail = Some("Downloading the signed release package…".to_string());
        release
    };
    notify_state_changed();

    let version = release.version.to_string();
    crate::log_update!(info, %version, bytes = release.package.bytes, "Downloading update");
    crate::backend::bridge::RUNTIME.spawn(async move {
        let result = download_verified_package(&release).await;
        DOWNLOAD_IN_FLIGHT.store(false, Ordering::Release);
        match result {
            Ok(path) => {
                crate::log_update!(info, %version, "Update package verified and staged");
                {
                    let mut state = state();
                    state.kind = UpdateStateKind::Ready;
                    state.downloaded_path = Some(path);
                    state.detail = Some("Package verified and ready to install".to_string());
                }
                notify_state_changed();
                crate::backend::push_ui_action(crate::backend::UiAction::ShowSuccessToast {
                    message: "Echo update verified".to_string(),
                    detail: Some(
                        "Return to Settings > General > Update checks and choose Install & restart"
                            .to_string(),
                    ),
                });
            }
            Err(error) => {
                log_update_failure("Update download", &error);
                let mut state = state();
                state.kind = UpdateStateKind::Failed;
                state.downloaded_path = None;
                state.detail = Some(error.clone());
                drop(state);
                notify_state_changed();
                crate::backend::push_ui_action(crate::backend::UiAction::ShowWarningToast {
                    message: "Update download failed".to_string(),
                    detail: Some(error),
                });
            }
        }
    });
}

/// Launch a byte-for-byte staged copy of the running Echo executable in its
/// UI-free updater mode. The caller should request a clean app shutdown only
/// after this returns success.
pub fn launch_installer() -> Result<(), String> {
    let result = start_installer();
    match &result {
        Ok(()) => crate::log_update!(info, "Update installer launched; Echo will restart"),
        Err(error) => log_update_failure("Update install", error),
    }
    result
}

/// Integrity failures are errors; transient or environmental ones are warnings.
fn log_update_failure(stage: &str, error: &str) {
    if error == SIGNATURE_VERIFICATION_FAILED || error == PACKAGE_SHA256_MISMATCH {
        crate::log_update!(error, platform = platform_id(), "{stage} failed: {error}");
    } else {
        crate::log_update!(warn, platform = platform_id(), "{stage} failed: {error}");
    }
}

const SIGNATURE_VERIFICATION_FAILED: &str = echo_update_core::SIGNATURE_VERIFICATION_FAILED;
const PACKAGE_SHA256_MISMATCH: &str = "Downloaded update SHA-256 verification failed";

fn start_installer() -> Result<(), String> {
    #[cfg(not(target_os = "windows"))]
    {
        Err("Automatic replacement is currently available for Windows packages only".to_string())
    }

    #[cfg(target_os = "windows")]
    {
        let (source, envelope) = {
            let state = state();
            let source = state
                .downloaded_path
                .clone()
                .ok_or_else(|| "No verified update has been downloaded".to_string())?;
            let envelope = state
                .release
                .as_ref()
                .map(|release| Arc::clone(&release.envelope))
                .ok_or_else(|| "Verified release metadata is unavailable".to_string())?;
            (source, envelope)
        };
        let manifest = source.with_extension("manifest.json");
        std::fs::write(&manifest, &envelope[..])
            .map_err(|error| format!("Could not stage the signed update manifest: {error}"))?;
        let target = std::env::current_exe()
            .map_err(|error| format!("Cannot resolve the running Echo executable: {error}"))?;
        let directory = target
            .parent()
            .ok_or_else(|| "Echo's installation directory is unavailable".to_string())?;

        if !install_is_replaceable(&target, directory)? {
            // Elevate the installed executable itself: it lives in the
            // protected install directory, so neither it nor the DLLs beside
            // it can be planted by a user-level process. It relaunches Echo
            // through the desktop shell so the new Echo is not elevated.
            let arguments = installer_arguments(&source, &target, &manifest, "shell");
            return launch_installer_elevated(&target, directory, &arguments);
        }

        let helper = staged_installer_install_path(&target)
            .ok_or_else(|| "Echo's installation directory is unavailable".to_string())?;
        stage_installer_copy(&target, &helper).map_err(|error| {
            format!("Could not stage Echo's updater beside the installation: {error}")
        })?;
        let arguments = installer_arguments(&source, &target, &manifest, "direct");
        let mut command = Command::new(&helper);
        command.args(&arguments).current_dir(directory);
        crate::backend::commands::strip_logfile_relaunch_env(&mut command);
        command.spawn().map(|_| ()).map_err(|error| {
            format!(
                "Failed to start staged updater {}: {error}",
                helper.display()
            )
        })
    }
}

/// Whether the current user can both create files in Echo's install directory
/// and delete (rename away) the installed executable, which is what an
/// unelevated update does. Only an access-denied result means elevation is
/// needed; any other failure aborts the update.
#[cfg(target_os = "windows")]
fn install_is_replaceable(executable: &Path, directory: &Path) -> Result<bool, String> {
    use std::os::windows::fs::OpenOptionsExt;
    const DELETE: u32 = 0x0001_0000;
    const FILE_SHARE_READ_WRITE_DELETE: u32 = 0x7;

    let can_create = loop {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.subsec_nanos());
        let probe = directory.join(format!(
            ".echo-write-probe-{}-{nanos:09}.tmp",
            std::process::id()
        ));
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&probe)
        {
            Ok(file) => {
                drop(file);
                std::fs::remove_file(&probe).map_err(|error| {
                    format!("Could not remove the install-directory write probe: {error}")
                })?;
                break true;
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => break false,
            Err(error) => {
                return Err(format!(
                    "Could not check whether Echo's install directory is writable: {error}"
                ));
            }
        }
    };
    if !can_create {
        return Ok(false);
    }
    match std::fs::OpenOptions::new()
        .access_mode(DELETE)
        .share_mode(FILE_SHARE_READ_WRITE_DELETE)
        .open(executable)
    {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => Ok(false),
        Err(error) => Err(format!(
            "Could not check whether Echo's executable can be replaced: {error}"
        )),
    }
}

/// Remove the side-by-side self-installer or a legacy cache-resident one after
/// a successful relaunch. The new process can race the old helper's final few
/// instructions, so retry only these exact filenames for a bounded interval.
/// The elevated relay copy gets one silent attempt: it sits in an admin-only
/// directory, is scheduled for removal at reboot, and is replaced by the next
/// elevated update.
pub fn start_staged_installer_cleanup() {
    #[cfg(target_os = "windows")]
    {
        let mut paths = Vec::with_capacity(2);
        if let Ok(running) = std::env::current_exe()
            && let Some(path) = staged_installer_install_path(&running)
        {
            paths.push(path);
        }
        if let Ok(path) = staged_installer_cache_path()
            && !paths.contains(&path)
        {
            paths.push(path);
        }
        if let Ok(running) = std::env::current_exe()
            && let Some(directory) = running.parent()
        {
            // One silent attempt: it only succeeds when Echo can write there.
            let _ = std::fs::remove_file(directory.join(echo_update_core::ELEVATED_RELAY_NAME));
        }
        paths.retain(|path| path.is_file());
        if paths.is_empty() {
            return;
        }
        std::thread::spawn(move || {
            for path in paths {
                let mut removed = false;
                for _ in 0..120 {
                    match std::fs::remove_file(&path) {
                        Ok(()) => {
                            removed = true;
                            break;
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                            removed = true;
                            break;
                        }
                        Err(_) => std::thread::sleep(Duration::from_millis(250)),
                    }
                }
                if !removed {
                    crate::log_ui!(warn, path = %path.display(), "Could not remove staged update helper");
                }
            }
        });
    }
}

#[cfg(target_os = "windows")]
fn stage_installer_copy(running_executable: &Path, helper: &Path) -> Result<(), String> {
    let directory = helper
        .parent()
        .ok_or_else(|| "Update helper directory is unavailable".to_string())?;
    std::fs::create_dir_all(directory)
        .map_err(|error| format!("Could not create the update helper directory: {error}"))?;
    let temporary = directory.join(format!(
        ".{STAGED_INSTALLER_NAME}.{}.tmp",
        std::process::id()
    ));
    if temporary.exists() {
        std::fs::remove_file(&temporary)
            .map_err(|error| format!("Could not clear a stale updater copy: {error}"))?;
    }
    std::fs::copy(running_executable, &temporary)
        .map_err(|error| format!("Could not stage Echo's built-in updater: {error}"))?;
    std::fs::OpenOptions::new()
        .write(true)
        .open(&temporary)
        .and_then(|file| file.sync_all())
        .map_err(|error| format!("Could not flush Echo's built-in updater: {error}"))?;

    let running_hash = sha256_file(running_executable)?;
    let staged_hash = sha256_file(&temporary)?;
    if running_hash != staged_hash {
        let _ = std::fs::remove_file(&temporary);
        return Err("Echo's staged updater copy failed its SHA-256 check".to_string());
    }
    if helper.exists() {
        std::fs::remove_file(helper).map_err(|error| {
            format!(
                "A previous Echo update helper is still active or could not be replaced: {error}"
            )
        })?;
    }
    std::fs::rename(&temporary, helper)
        .map_err(|error| format!("Could not publish Echo's staged updater: {error}"))?;
    Ok(())
}

#[cfg(target_os = "windows")]
fn staged_installer_install_path(running_executable: &Path) -> Option<PathBuf> {
    running_executable
        .parent()
        .map(|directory| directory.join(STAGED_INSTALLER_NAME))
}

#[cfg(target_os = "windows")]
fn staged_installer_cache_path() -> Result<PathBuf, String> {
    Ok(update_cache_directory()?.join(STAGED_INSTALLER_NAME))
}

#[cfg(target_os = "windows")]
fn installer_arguments(
    source: &Path,
    target: &Path,
    manifest: &Path,
    relaunch: &str,
) -> Vec<OsString> {
    vec![
        OsString::from("apply-update"),
        OsString::from("--parent-pid"),
        OsString::from(std::process::id().to_string()),
        OsString::from("--source"),
        source.as_os_str().to_owned(),
        OsString::from("--target"),
        target.as_os_str().to_owned(),
        OsString::from("--manifest"),
        manifest.as_os_str().to_owned(),
        OsString::from("--relaunch"),
        OsString::from(relaunch),
    ]
}

#[cfg(target_os = "windows")]
fn launch_installer_elevated(
    installed_executable: &Path,
    directory: &Path,
    arguments: &[OsString],
) -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt;
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::SW_HIDE;
    use windows::core::PCWSTR;

    let verb: Vec<u16> = OsStr::new("runas").encode_wide().chain(Some(0)).collect();
    let file: Vec<u16> = installed_executable
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect();
    let parameters = quote_windows_arguments(arguments);
    let parameters: Vec<u16> = OsStr::new(&parameters)
        .encode_wide()
        .chain(Some(0))
        .collect();
    let directory: Vec<u16> = directory.as_os_str().encode_wide().chain(Some(0)).collect();
    // SAFETY: every string argument is a NUL-terminated UTF-16 buffer that outlives the call.
    let result = unsafe {
        ShellExecuteW(
            None,
            PCWSTR(verb.as_ptr()),
            PCWSTR(file.as_ptr()),
            PCWSTR(parameters.as_ptr()),
            PCWSTR(directory.as_ptr()),
            SW_HIDE,
        )
    };
    let code = result.0 as isize;
    if code > 32 {
        Ok(())
    } else {
        Err(format!(
            "Windows could not start the updater with administrator consent (ShellExecute code {code})"
        ))
    }
}

#[cfg(target_os = "windows")]
fn quote_windows_arguments(arguments: &[OsString]) -> String {
    arguments
        .iter()
        .map(|argument| quote_windows_argument(argument.as_os_str()))
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(target_os = "windows")]
fn quote_windows_argument(argument: &OsStr) -> String {
    let value = argument.to_string_lossy();
    let mut quoted = String::with_capacity(value.len() + 2);
    quoted.push('"');
    let mut backslashes = 0usize;
    for character in value.chars() {
        match character {
            '\\' => backslashes += 1,
            '"' => {
                quoted.extend(std::iter::repeat_n('\\', backslashes * 2 + 1));
                quoted.push('"');
                backslashes = 0;
            }
            _ => {
                quoted.extend(std::iter::repeat_n('\\', backslashes));
                backslashes = 0;
                quoted.push(character);
            }
        }
    }
    quoted.extend(std::iter::repeat_n('\\', backslashes * 2));
    quoted.push('"');
    quoted
}

async fn fetch_verified_release() -> Result<Option<VerifiedRelease>, String> {
    let client = update_client(MANIFEST_CONNECT_TIMEOUT, MANIFEST_REQUEST_TIMEOUT)?;
    let manifest_url = format!("{UPDATE_ORIGIN}/stable/{}/manifest.json", platform_id());
    let response = crate::backend::http::send_logged(
        crate::backend::http::LogCategory::Update,
        client.get(manifest_url),
    )
    .await
    .map_err(|error| {
        format!(
            "Could not reach the update service: {}",
            error.without_url()
        )
    })?;
    if response.status() == StatusCode::NOT_FOUND {
        return Ok(None);
    }
    require_success(&response, "Update manifest")?;
    let bytes = read_limited(response, MAX_MANIFEST_BYTES).await?;
    let release = verify_envelope(bytes)?;

    Ok(Some(release))
}

fn verify_envelope(bytes: Vec<u8>) -> Result<VerifiedRelease, String> {
    let payload = echo_update_core::verify_envelope_payload(&bytes)?;
    let manifest: ManifestPayload = serde_json::from_slice(&payload)
        .map_err(|error| format!("Signed update payload is invalid: {error}"))?;
    validate_manifest(manifest, bytes.into())
}

fn validate_manifest(
    manifest: ManifestPayload,
    envelope: Arc<[u8]>,
) -> Result<VerifiedRelease, String> {
    if manifest.schema_version != 1 || manifest.channel != "stable" {
        return Err("Update manifest schema or channel is unsupported".to_string());
    }
    let version = semver::Version::parse(manifest.version.trim())
        .map_err(|error| format!("Update version is invalid: {error}"))?;
    let published_at = manifest.published_at.trim().to_string();
    if published_at.is_empty() || published_at.len() > 64 {
        return Err("Update publication timestamp is invalid".to_string());
    }
    let changelog = manifest.changelog.trim().to_string();
    if changelog.is_empty() || changelog.len() > MAX_CHANGELOG_BYTES {
        return Err("Update changelog is empty or exceeds the size limit".to_string());
    }
    if manifest.package.platform != platform_id() {
        return Err(format!(
            "Update package targets {}, but this build requires {}",
            manifest.package.platform,
            platform_id()
        ));
    }
    if manifest.package.bytes == 0 || manifest.package.bytes > MAX_PACKAGE_BYTES {
        return Err("Update package size is outside the accepted range".to_string());
    }
    let sha256 = manifest.package.sha256.trim().to_ascii_lowercase();
    if sha256.len() != 64 || !sha256.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("Update package SHA-256 is invalid".to_string());
    }
    let url = url::Url::parse(manifest.package.url.trim())
        .map_err(|error| format!("Update package URL is invalid: {error}"))?;
    if url.scheme() != "https"
        || url.host_str() != Some(UPDATE_HOST)
        || !url.path().starts_with(UPDATE_PATH_PREFIX)
        || url.username() != ""
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return Err("Update package URL is outside Echo's trusted HTTPS origin".to_string());
    }

    Ok(VerifiedRelease {
        version,
        published_at,
        changelog,
        package: VerifiedPackage {
            url: url.to_string(),
            bytes: manifest.package.bytes,
            sha256,
        },
        envelope,
    })
}

async fn download_verified_package(release: &VerifiedRelease) -> Result<PathBuf, String> {
    let directory = update_cache_directory()?;
    std::fs::create_dir_all(&directory)
        .map_err(|error| format!("Could not create update cache: {error}"))?;
    let extension = if cfg!(target_os = "windows") {
        "exe"
    } else {
        "bin"
    };
    let final_path = directory.join(format!(
        "echo-{}-{}.{}",
        release.version,
        &release.package.sha256[..12],
        extension
    ));
    if file_matches_package(&final_path, &release.package)? {
        remove_other_update_packages(&directory, &final_path, extension);
        return Ok(final_path);
    }

    let temp_path = directory.join(format!(
        ".echo-{}-{}-{}.download",
        release.version,
        std::process::id(),
        &release.package.sha256[..8]
    ));
    let client = update_client(PACKAGE_CONNECT_TIMEOUT, PACKAGE_REQUEST_TIMEOUT)?;
    let response = crate::backend::http::send_logged(
        crate::backend::http::LogCategory::Update,
        client.get(&release.package.url),
    )
    .await
    .map_err(|error| format!("Could not download the update: {}", error.without_url()))?;
    require_success(&response, "Update package")?;
    if response
        .content_length()
        .is_some_and(|length| length != release.package.bytes)
    {
        return Err("Update package Content-Length does not match the signed manifest".to_string());
    }
    let download_result: Result<(), String> = async {
        let mut response = response;
        let mut file = tokio::fs::File::create(&temp_path)
            .await
            .map_err(|error| format!("Could not create the update staging file: {error}"))?;
        let mut received = 0u64;
        let mut hash = digest::Context::new(&digest::SHA256);
        while let Some(chunk) = response.chunk().await.map_err(|error| {
            format!(
                "Failed while downloading the update: {}",
                error.without_url()
            )
        })? {
            received = received
                .checked_add(chunk.len() as u64)
                .ok_or_else(|| "Downloaded update size overflowed".to_string())?;
            if received > release.package.bytes || received > MAX_PACKAGE_BYTES {
                return Err("Downloaded update exceeds the signed size".to_string());
            }
            hash.update(&chunk);
            file.write_all(&chunk)
                .await
                .map_err(|error| format!("Could not stage the downloaded update: {error}"))?;
        }
        if received != release.package.bytes {
            return Err("Downloaded update size does not match the signed manifest".to_string());
        }
        if bytes_to_hex(hash.finish().as_ref()) != release.package.sha256 {
            return Err(PACKAGE_SHA256_MISMATCH.to_string());
        }
        file.flush()
            .await
            .map_err(|error| format!("Could not flush the staged update: {error}"))?;
        file.sync_all()
            .await
            .map_err(|error| format!("Could not persist the staged update: {error}"))?;
        Ok(())
    }
    .await;
    if let Err(error) = download_result {
        let _ = tokio::fs::remove_file(&temp_path).await;
        return Err(error);
    }
    if final_path.exists() {
        std::fs::remove_file(&final_path)
            .map_err(|error| format!("Could not replace the stale update cache entry: {error}"))?;
    }
    std::fs::rename(&temp_path, &final_path)
        .map_err(|error| format!("Could not publish the verified update locally: {error}"))?;
    remove_other_update_packages(&directory, &final_path, extension);
    Ok(final_path)
}

/// Delete what earlier updates left in the update cache: packages
/// (`echo-{version}-{sha}.{ext}`), their staged manifests and abandoned
/// `.download` files. Only the newest package is ever run. The staged helper
/// shares the folder and the `echo-` prefix but not a leading version digit.
fn remove_other_update_packages(directory: &Path, keep: &Path, extension: &str) {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return;
    };
    let keep_manifest = keep.with_extension("manifest.json");
    let package_suffix = format!(".{extension}");
    for entry in entries.flatten() {
        let path = entry.path();
        if path == keep || path == keep_manifest {
            continue;
        }
        let stale = path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| {
                let versioned = |prefix: &str| {
                    name.strip_prefix(prefix)
                        .is_some_and(|rest| rest.starts_with(|c: char| c.is_ascii_digit()))
                };
                (versioned("echo-")
                    && (name.ends_with(&package_suffix) || name.ends_with(".manifest.json")))
                    || (versioned(".echo-") && name.ends_with(".download"))
            });
        if stale {
            let _ = std::fs::remove_file(&path);
        }
    }
}

fn update_client(
    connect_timeout: Duration,
    request_timeout: Duration,
) -> Result<reqwest::Client, String> {
    crate::backend::http::client_builder()
        .connect_timeout(connect_timeout)
        .timeout(request_timeout)
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|error| format!("Could not initialize the update client: {error}"))
}

fn require_success(response: &reqwest::Response, label: &str) -> Result<(), String> {
    if response.status().is_success() {
        Ok(())
    } else {
        Err(format!("{label} returned HTTP {}", response.status()))
    }
}

async fn read_limited(mut response: reqwest::Response, limit: usize) -> Result<Vec<u8>, String> {
    if response
        .content_length()
        .is_some_and(|length| length > limit as u64)
    {
        return Err(format!("HTTP response exceeds the {limit}-byte limit"));
    }
    let mut bytes =
        Vec::with_capacity(response.content_length().unwrap_or(0).min(limit as u64) as usize);
    while let Some(chunk) = response.chunk().await.map_err(|error| {
        format!(
            "Failed while reading the update response: {}",
            error.without_url()
        )
    })? {
        if bytes.len().saturating_add(chunk.len()) > limit {
            return Err(format!("HTTP response exceeds the {limit}-byte limit"));
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

fn file_matches_package(path: &Path, package: &VerifiedPackage) -> Result<bool, String> {
    if !path.is_file() {
        return Ok(false);
    }
    let metadata = std::fs::metadata(path)
        .map_err(|error| format!("Could not inspect cached update: {error}"))?;
    if metadata.len() != package.bytes {
        return Ok(false);
    }
    Ok(sha256_file(path)? == package.sha256)
}

fn sha256_file(path: &Path) -> Result<String, String> {
    use std::io::Read;

    let mut file = std::fs::File::open(path)
        .map_err(|error| format!("Could not open cached update for hashing: {error}"))?;
    let mut hash = digest::Context::new(&digest::SHA256);
    let mut buffer = vec![0u8; 1024 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|error| format!("Could not hash cached update: {error}"))?;
        if read == 0 {
            break;
        }
        hash.update(&buffer[..read]);
    }
    Ok(bytes_to_hex(hash.finish().as_ref()))
}

fn bytes_to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn update_cache_directory() -> Result<PathBuf, String> {
    crate::benchmark_profile::cache_root()
        .map(|path| path.join("updates"))
        .ok_or_else(|| "The operating system did not provide a cache directory".to_string())
}

fn platform_id() -> &'static str {
    echo_update_core::platform_id()
}
