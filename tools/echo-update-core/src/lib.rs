//! Shared, UI-free Windows update installation and rollback implementation.
//!
//! Both Echo's staged self-copy and the compatibility `echo-version` helper
//! enter this code before initializing the GPUI application.
//!
//! The updater may run elevated behind a genuine Echo consent prompt, so it
//! trusts nothing on its command line: the package hash, size, and version
//! come from the Ed25519-signed update manifest, and the file it replaces must
//! be the Echo executable the updater itself was started from.

use base64::Engine;
use ring::{digest, signature};
use serde::Deserialize;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Ed25519 key that signs Echo's stable update manifests.
pub const UPDATE_PUBLIC_KEY: [u8; 32] = [
    0x7a, 0x56, 0xf1, 0xb0, 0x9e, 0x14, 0xe9, 0x9c, 0xbe, 0x78, 0xf3, 0xfb, 0x4c, 0xb1, 0x31, 0xcb,
    0x66, 0x24, 0x99, 0xfc, 0x51, 0x47, 0x43, 0xec, 0x92, 0x1c, 0x15, 0x81, 0x91, 0x47, 0x98, 0xcb,
];

pub const SIGNATURE_VERIFICATION_FAILED: &str = "Update manifest signature verification failed";
pub const MAX_MANIFEST_BYTES: usize = 256 * 1024;
pub const MAX_PACKAGE_BYTES: u64 = 256 * 1024 * 1024;

/// File name of the admin-only copy an elevated update re-executes from, so
/// the installed executable is not running while it is replaced.
pub const ELEVATED_RELAY_NAME: &str = "echo-updater.exe";

/// File name of the unelevated self-copy Echo stages beside a writable
/// installation to replace the installed executable.
pub const STAGED_HELPER_NAME: &str = "echo-self-updater.exe";

/// File name of the standalone compatibility updater (`tools/echo-version`).
const COMPATIBILITY_HELPER_NAME: &str = "echo-version.exe";

struct ApplyArguments {
    parent_pid: u32,
    source: PathBuf,
    target: PathBuf,
    manifest: PathBuf,
    relaunch: Relaunch,
}

/// How the updated Echo is started once installed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Relaunch {
    /// As a child of the updater, with the updater's token.
    Direct,
    /// Through the desktop shell (Explorer), so an elevated updater starts
    /// Echo with the signed-in user's normal, unelevated token.
    Shell,
}

/// Which step of an update this process performs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stage {
    /// The installed executable itself (the elevated entry point). It hands
    /// the update to an [`ELEVATED_RELAY_NAME`] copy of itself.
    Installed,
    /// The elevated relay copy beside the installed executable.
    Relay,
    /// The unelevated [`STAGED_HELPER_NAME`] copy beside a writable install.
    Helper,
    /// The unelevated [`COMPATIBILITY_HELPER_NAME`] entry point.
    Compatibility,
}

/// The package a verified, signed manifest allows this updater to install.
#[derive(Debug, PartialEq, Eq)]
struct SignedPackage {
    version: semver::Version,
    bytes: u64,
    sha256: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SignedEnvelope {
    schema_version: u32,
    payload: String,
    signature: String,
}

#[derive(Deserialize)]
struct SignedPayload {
    schema_version: u32,
    channel: String,
    version: String,
    package: SignedPayloadPackage,
}

#[derive(Deserialize)]
struct SignedPayloadPackage {
    platform: String,
    bytes: u64,
    sha256: String,
}

/// Apply a staged update. Every failure is appended to `echo-update.log`
/// beside the installed executable, or beside the running executable (then
/// `%TEMP%`) when the target directory is not known yet.
pub fn apply_update(arguments: &[String]) -> Result<(), String> {
    let mut log_path = fallback_log_path();
    let result = apply_update_logged(arguments, &mut log_path);
    if let Err(error) = &result {
        append_log(&log_path, &format!("Update failed: {error}"));
    }
    result
}

/// The update platform this build installs packages for.
/// How often a running Echo re-checks the signed manifest after a check that
/// found no newer release. Echo is often left running for days, and a check
/// only at startup would never tell it about a release.
pub const RECHECK_INTERVAL: Duration = Duration::from_secs(4 * 60 * 60);
/// How soon an automatic check that failed (offline, update origin down) is
/// retried.
pub const RECHECK_AFTER_FAILURE: Duration = Duration::from_secs(30 * 60);

/// The update state an automatic re-check decision depends on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecheckState {
    /// A check is running, or a release was already offered, downloaded or
    /// staged: the user has the news, so there is nothing to re-check.
    Busy,
    /// The last check finished without offering a release.
    Settled,
    /// The last check failed.
    Failed,
}

/// Whether an automatic re-check is due, `since_last_check` after the last
/// check. `interval` is the settled re-check interval ([`RECHECK_INTERVAL`]
/// unless overridden for testing); failed checks retry after
/// [`RECHECK_AFTER_FAILURE`] or `interval`, whichever is sooner.
pub fn automatic_recheck_due(
    state: RecheckState,
    since_last_check: Duration,
    interval: Duration,
) -> bool {
    match state {
        RecheckState::Busy => false,
        RecheckState::Settled => since_last_check >= interval,
        RecheckState::Failed => since_last_check >= interval.min(RECHECK_AFTER_FAILURE),
    }
}

pub fn platform_id() -> &'static str {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("windows", "x86_64") => "windows-x86_64",
        ("linux", "x86_64") => "linux-x86_64",
        ("linux", "aarch64") => "linux-aarch64",
        ("macos", "x86_64") => "macos-x86_64",
        ("macos", "aarch64") => "macos-aarch64",
        _ => "unsupported",
    }
}

/// Verify a signed update envelope with [`UPDATE_PUBLIC_KEY`] and return the
/// exact signed payload bytes.
pub fn verify_envelope_payload(bytes: &[u8]) -> Result<Vec<u8>, String> {
    verify_envelope_payload_with_key(bytes, &UPDATE_PUBLIC_KEY)
}

fn verify_envelope_payload_with_key(bytes: &[u8], public_key: &[u8]) -> Result<Vec<u8>, String> {
    let envelope: SignedEnvelope = serde_json::from_slice(bytes)
        .map_err(|error| format!("Update envelope is invalid JSON: {error}"))?;
    if envelope.schema_version != 1 {
        return Err(format!(
            "Unsupported update envelope schema {}",
            envelope.schema_version
        ));
    }
    let payload = base64::engine::general_purpose::STANDARD
        .decode(envelope.payload.trim())
        .map_err(|_| "Update envelope payload is not valid base64".to_string())?;
    if payload.len() > MAX_MANIFEST_BYTES {
        return Err("Signed update payload exceeds the size limit".to_string());
    }
    let signature_bytes = base64::engine::general_purpose::STANDARD
        .decode(envelope.signature.trim())
        .map_err(|_| "Update envelope signature is not valid base64".to_string())?;
    signature::UnparsedPublicKey::new(&signature::ED25519, public_key)
        .verify(&payload, &signature_bytes)
        .map_err(|_| SIGNATURE_VERIFICATION_FAILED.to_string())?;
    Ok(payload)
}

/// The package a verified payload allows: a stable release for this platform
/// that is strictly newer than `running_version`.
fn signed_package(payload: &[u8], running_version: &str) -> Result<SignedPackage, String> {
    let payload: SignedPayload = serde_json::from_slice(payload)
        .map_err(|error| format!("Signed update payload is invalid: {error}"))?;
    if payload.schema_version != 1 || payload.channel != "stable" {
        return Err("Update manifest schema or channel is unsupported".to_string());
    }
    if payload.package.platform != platform_id() {
        return Err("Signed update targets a different platform".to_string());
    }
    let version = semver::Version::parse(payload.version.trim())
        .map_err(|error| format!("Update version is invalid: {error}"))?;
    let running = semver::Version::parse(running_version)
        .map_err(|error| format!("Running Echo version is invalid: {error}"))?;
    if version <= running {
        return Err(format!(
            "Signed update {version} is not newer than the installed Echo {running}"
        ));
    }
    if payload.package.bytes == 0 || payload.package.bytes > MAX_PACKAGE_BYTES {
        return Err("Update package size is outside the accepted range".to_string());
    }
    let sha256 = payload.package.sha256.trim().to_ascii_lowercase();
    if sha256.len() != 64 || !sha256.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("Update package SHA-256 is invalid".to_string());
    }
    Ok(SignedPackage {
        version,
        bytes: payload.package.bytes,
        sha256,
    })
}

fn fallback_log_path() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_path_buf))
        .filter(|directory| directory.is_dir())
        .unwrap_or_else(std::env::temp_dir)
        .join(LOG_FILE_NAME)
}

const LOG_FILE_NAME: &str = "echo-update.log";

fn apply_update_logged(arguments: &[String], log_path: &mut PathBuf) -> Result<(), String> {
    let arguments = parse_apply_arguments(arguments)?;
    let elevated = process_is_elevated();
    check_relaunch_mode(elevated, arguments.relaunch)?;
    let source = absolute_path(&arguments.source, "source")?;
    let target = canonical_path(&arguments.target, "target")?;
    let running = std::env::current_exe()
        .and_then(fs::canonicalize)
        .map(|path| simplify_verbatim(&path))
        .map_err(|error| format!("Cannot resolve the running updater: {error}"))?;
    let stage = classify_stage(&running, &target, elevated, || {
        Ok(sha256_file(&running)? == sha256_file(&target)?)
    })?;
    let target_directory = target
        .parent()
        .ok_or_else(|| "Installed Echo directory is unavailable".to_string())?;
    *log_path = target_directory.join(LOG_FILE_NAME);
    let log_path = log_path.as_path();
    append_log(
        log_path,
        &format!(
            "Starting update from {} for parent PID {}",
            source.display(),
            arguments.parent_pid
        ),
    );

    let manifest = read_update_file(&arguments.manifest, MAX_MANIFEST_BYTES as u64, "manifest")?;
    let package = signed_package(
        &verify_envelope_payload(&manifest)?,
        env!("CARGO_PKG_VERSION"),
    )?;
    let payload = read_update_file(&source, package.bytes, "package")?;
    if payload.len() as u64 != package.bytes || sha256_hex(&payload) != package.sha256 {
        return Err("Staged update failed the installer's SHA-256 recheck".to_string());
    }

    if stage == Stage::Installed {
        return start_relay(&arguments, &target, target_directory, log_path);
    }

    let install_timeout = Duration::from_secs(90);
    let install_started = Instant::now();
    wait_for_parent_exit(arguments.parent_pid, install_timeout)?;
    append_log(log_path, "Parent Echo process exited");

    let backup = sibling_path(&target, "previous")?;
    let staged = sibling_path(&target, "new")?;
    write_staged_payload(&payload, &staged)?;
    drop(payload);
    let replace_timeout = install_timeout.saturating_sub(install_started.elapsed());
    if let Err(error) = wait_for_replaceable_target(&target, &backup, replace_timeout) {
        let _ = fs::remove_file(&staged);
        return Err(error);
    }
    append_log(log_path, "Installed executable moved to the rollback path");

    if let Err(error) = install_staged(&staged, &target, &package.sha256) {
        let _ = fs::remove_file(&staged);
        let rollback = rollback(&target, &backup);
        return Err(match rollback {
            Ok(()) => format!("{error}; the previous Echo executable was restored"),
            Err(rollback_error) => format!("{error}; rollback also failed: {rollback_error}"),
        });
    }

    if arguments.relaunch == Relaunch::Shell {
        match launch_through_shell(&target, target_directory) {
            Ok(()) => append_log(
                log_path,
                &format!(
                    "Installed verified Echo {} ({}) and relaunched it unelevated",
                    package.version, package.sha256
                ),
            ),
            Err(error) => append_log(
                log_path,
                &format!(
                    "Installed verified Echo {} ({}); could not relaunch it unelevated ({error}), start Echo manually",
                    package.version, package.sha256
                ),
            ),
        }
        schedule_relay_removal(log_path);
        return Ok(());
    }

    match Command::new(&target).current_dir(target_directory).spawn() {
        Ok(_) => {
            append_log(
                log_path,
                &format!(
                    "Installed and launched verified Echo {} ({})",
                    package.version, package.sha256
                ),
            );
            Ok(())
        }
        Err(error) => {
            let rollback_result = rollback(&target, &backup);
            if rollback_result.is_ok() {
                let relaunch = match Command::new(&target).current_dir(target_directory).spawn() {
                    Ok(_) => "relaunched the previous Echo".to_string(),
                    Err(relaunch_error) => {
                        format!("could not relaunch the previous Echo: {relaunch_error}")
                    }
                };
                append_log(log_path, &format!("After rollback: {relaunch}"));
            }
            Err(match rollback_result {
                Ok(()) => format!(
                    "Updated Echo would not launch ({error}); the previous executable was restored"
                ),
                Err(rollback_error) => format!(
                    "Updated Echo would not launch ({error}); rollback failed: {rollback_error}"
                ),
            })
        }
    }
}

/// An elevated updater must start the new Echo through the desktop shell, or
/// Echo itself would keep running with administrator rights.
fn check_relaunch_mode(elevated: bool, relaunch: Relaunch) -> Result<(), String> {
    if elevated && relaunch == Relaunch::Direct {
        return Err("An elevated update must relaunch Echo through the desktop shell".to_string());
    }
    Ok(())
}

/// Decide which [`Stage`] `running` performs for `target` (both canonical),
/// rejecting targets that are not the Echo executable this updater came from.
/// `identical` reports whether both files have the same contents; it is only
/// consulted for the relay and helper copies.
fn classify_stage(
    running: &Path,
    target: &Path,
    elevated: bool,
    identical: impl FnOnce() -> Result<bool, String>,
) -> Result<Stage, String> {
    if running == target {
        return Ok(Stage::Installed);
    }
    let running_name = running.file_name().and_then(|name| name.to_str());
    let stage = match running_name {
        Some(name) if name.eq_ignore_ascii_case(ELEVATED_RELAY_NAME) => Stage::Relay,
        Some(name) if name.eq_ignore_ascii_case(STAGED_HELPER_NAME) => Stage::Helper,
        Some(name) if !elevated && name.eq_ignore_ascii_case(COMPATIBILITY_HELPER_NAME) => {
            return Ok(Stage::Compatibility);
        }
        _ => {
            return Err(
                "An update may only replace the Echo executable the updater runs from".to_string(),
            );
        }
    };
    if running.parent().is_none() || running.parent() != target.parent() {
        return Err("The update target is not beside the updater copy".to_string());
    }
    let target_name = target
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    let is_updater_copy = target_name.eq_ignore_ascii_case(ELEVATED_RELAY_NAME)
        || target_name.eq_ignore_ascii_case(STAGED_HELPER_NAME);
    let is_executable = target_name.len() > 4
        && target_name
            .get(target_name.len() - 4..)
            .is_some_and(|extension| extension.eq_ignore_ascii_case(".exe"));
    if is_updater_copy || !is_executable {
        return Err("The update target is not an installed Echo executable".to_string());
    }
    if !identical()? {
        return Err(
            "The update target is not the Echo executable this updater was copied from".to_string(),
        );
    }
    Ok(stage)
}

/// Whether this process runs with an elevated (administrator) token. Any
/// failure to tell counts as elevated, which only makes the checks stricter.
#[cfg(windows)]
fn process_is_elevated() -> bool {
    use windows::Win32::Foundation::{CloseHandle, HANDLE};
    use windows::Win32::Security::{
        GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation,
    };
    use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    let mut token = HANDLE::default();
    // SAFETY: the process pseudo-handle is always valid, and `token` is a valid out-parameter.
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) }.is_err() {
        return true;
    }
    let mut elevation = TOKEN_ELEVATION::default();
    let mut returned = 0u32;
    // SAFETY: `token` was opened above, and the buffer and size describe `elevation`.
    let queried = unsafe {
        GetTokenInformation(
            token,
            TokenElevation,
            Some(std::ptr::from_mut(&mut elevation).cast()),
            std::mem::size_of::<TOKEN_ELEVATION>() as u32,
            &mut returned,
        )
    };
    // SAFETY: `token` was opened above and is closed exactly once.
    let _ = unsafe { CloseHandle(token) };
    queried.is_err() || elevation.TokenIsElevated != 0
}

#[cfg(not(windows))]
fn process_is_elevated() -> bool {
    false
}

/// The installed executable cannot replace itself while running, so copy it
/// to [`ELEVATED_RELAY_NAME`] in the (admin-only) install directory and hand
/// the update to that copy. The copy inherits this process's token and waits
/// for this process to exit before replacing the target.
fn start_relay(
    arguments: &ApplyArguments,
    target: &Path,
    target_directory: &Path,
    log_path: &Path,
) -> Result<(), String> {
    wait_for_parent_exit(arguments.parent_pid, Duration::from_secs(90))?;
    let relay = target_directory.join(ELEVATED_RELAY_NAME);
    if relay.exists() {
        fs::remove_file(&relay)
            .map_err(|error| format!("Could not clear a previous update relay: {error}"))?;
    }
    fs::copy(target, &relay)
        .map_err(|error| format!("Could not stage the update relay: {error}"))?;
    if sha256_file(&relay)? != sha256_file(target)? {
        let _ = fs::remove_file(&relay);
        return Err("The update relay failed its SHA-256 check".to_string());
    }
    let relaunch = match arguments.relaunch {
        Relaunch::Direct => "direct",
        Relaunch::Shell => "shell",
    };
    Command::new(&relay)
        .arg("apply-update")
        .arg("--parent-pid")
        .arg(std::process::id().to_string())
        .arg("--source")
        .arg(&arguments.source)
        .arg("--target")
        .arg(target)
        .arg("--manifest")
        .arg(&arguments.manifest)
        .arg("--relaunch")
        .arg(relaunch)
        .current_dir(target_directory)
        .spawn()
        .map_err(|error| format!("Could not start the update relay: {error}"))?;
    append_log(log_path, "Handed the update to the relay copy");
    Ok(())
}

/// Delete the relay copy at the next reboot when this process is that copy.
/// A running executable cannot delete itself, and the unelevated Echo that
/// starts next cannot write to the install directory; the next elevated
/// update also replaces a leftover copy.
fn schedule_relay_removal(log_path: &Path) {
    let Ok(running) = std::env::current_exe() else {
        return;
    };
    if running.file_name().and_then(|name| name.to_str()) != Some(ELEVATED_RELAY_NAME) {
        return;
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        use windows::Win32::Storage::FileSystem::{MOVEFILE_DELAY_UNTIL_REBOOT, MoveFileExW};
        use windows::core::PCWSTR;

        let path: Vec<u16> = running.as_os_str().encode_wide().chain(Some(0)).collect();
        // SAFETY: `path` is a NUL-terminated UTF-16 buffer; a null destination schedules deletion at reboot.
        if let Err(error) = unsafe {
            MoveFileExW(
                PCWSTR(path.as_ptr()),
                PCWSTR::null(),
                MOVEFILE_DELAY_UNTIL_REBOOT,
            )
        } {
            append_log(
                log_path,
                &format!("Could not schedule removal of the update relay: {error}"),
            );
        }
    }
    #[cfg(not(windows))]
    let _ = log_path;
}

/// Start `target` through the desktop shell's `IShellDispatch2::ShellExecute`
/// (the Explorer process), which runs it with the signed-in user's unelevated
/// token even when called from an elevated process.
#[cfg(windows)]
fn launch_through_shell(target: &Path, directory: &Path) -> Result<(), String> {
    use windows::Win32::System::Com::{
        CLSCTX_LOCAL_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
        CoUninitialize, IServiceProvider,
    };
    use windows::Win32::System::Variant::VARIANT;
    use windows::Win32::UI::Shell::{
        IShellBrowser, IShellDispatch2, IShellFolderViewDual, IShellView, IShellWindows,
        SID_STopLevelBrowser, SVGIO_BACKGROUND, SWC_DESKTOP, SWFO_NEEDDISPATCH, ShellWindows,
    };
    use windows::core::{BSTR, Interface};

    // SAFETY: initializes COM for this thread; balanced by CoUninitialize below when it succeeds.
    let initialized = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }.is_ok();
    let result = (|| -> windows::core::Result<()> {
        // SAFETY: COM is initialized on this thread, and every interface is used only while the call chain keeps it alive.
        unsafe {
            let shell_windows: IShellWindows =
                CoCreateInstance(&ShellWindows, None, CLSCTX_LOCAL_SERVER)?;
            let desktop_location = VARIANT::from(0i32);
            let empty = VARIANT::default();
            let mut hwnd = 0i32;
            let desktop = shell_windows.FindWindowSW(
                &desktop_location,
                &empty,
                SWC_DESKTOP,
                &mut hwnd,
                SWFO_NEEDDISPATCH,
            )?;
            let browser: IShellBrowser = desktop
                .cast::<IServiceProvider>()?
                .QueryService(&SID_STopLevelBrowser)?;
            let view: IShellView = browser.QueryActiveShellView()?;
            let folder_view: IShellFolderViewDual = view.GetItemObject(SVGIO_BACKGROUND)?;
            let shell: IShellDispatch2 = folder_view.Application()?.cast()?;
            shell.ShellExecute(
                &BSTR::from(target.as_os_str().to_string_lossy().as_ref()),
                &VARIANT::default(),
                &VARIANT::from(BSTR::from(directory.as_os_str().to_string_lossy().as_ref())),
                &VARIANT::from(BSTR::from("open")),
                &VARIANT::from(1i32),
            )
        }
    })();
    if initialized {
        // SAFETY: balances the successful CoInitializeEx above.
        unsafe { CoUninitialize() };
    }
    result.map_err(|error| error.to_string())
}

#[cfg(not(windows))]
fn launch_through_shell(target: &Path, directory: &Path) -> Result<(), String> {
    Command::new(target)
        .current_dir(directory)
        .spawn()
        .map(|_| ())
        .map_err(|error| error.to_string())
}

#[cfg(windows)]
fn wait_for_parent_exit(parent_pid: u32, timeout: Duration) -> Result<(), String> {
    use std::ffi::c_void;

    type RawHandle = *mut c_void;

    const SYNCHRONIZE: u32 = 0x0010_0000;
    const WAIT_OBJECT_0: u32 = 0;
    const WAIT_TIMEOUT: u32 = 258;
    const WAIT_FAILED: u32 = u32::MAX;
    const ERROR_INVALID_PARAMETER: i32 = 87;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn OpenProcess(desired_access: u32, inherit_handle: i32, process_id: u32) -> RawHandle;
        fn WaitForSingleObject(handle: RawHandle, milliseconds: u32) -> u32;
        fn CloseHandle(handle: RawHandle) -> i32;
    }

    // SAFETY: OpenProcess takes only values and returns null on failure, which is checked.
    let handle = unsafe { OpenProcess(SYNCHRONIZE, 0, parent_pid) };
    if handle.is_null() {
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() == Some(ERROR_INVALID_PARAMETER) {
            return Ok(());
        }
        return Err(format!(
            "Could not wait for parent Echo process {parent_pid}: {error}"
        ));
    }

    let timeout_millis = timeout.as_millis().min(u128::from(u32::MAX - 1)) as u32;
    // SAFETY: `handle` is the non-null process handle opened above.
    let wait_result = unsafe { WaitForSingleObject(handle, timeout_millis) };
    let wait_error = (wait_result == WAIT_FAILED).then(std::io::Error::last_os_error);
    // SAFETY: `handle` was opened above and is closed exactly once.
    let _ = unsafe { CloseHandle(handle) };

    match wait_result {
        WAIT_OBJECT_0 => Ok(()),
        WAIT_TIMEOUT => Err(format!(
            "Echo process {parent_pid} did not exit within {} seconds",
            timeout.as_secs()
        )),
        WAIT_FAILED => Err(format!(
            "Could not wait for parent Echo process {parent_pid}: {}",
            wait_error.expect("WAIT_FAILED records the OS error")
        )),
        result => Err(format!(
            "Waiting for parent Echo process {parent_pid} returned unexpected status {result}"
        )),
    }
}

#[cfg(not(windows))]
fn wait_for_parent_exit(_parent_pid: u32, _timeout: Duration) -> Result<(), String> {
    Ok(())
}

fn parse_apply_arguments(arguments: &[String]) -> Result<ApplyArguments, String> {
    let mut parent_pid = None;
    let mut source = None;
    let mut target = None;
    let mut manifest = None;
    let mut relaunch = Relaunch::Direct;
    let mut index = 0;
    while index < arguments.len() {
        let value = arguments
            .get(index + 1)
            .ok_or_else(|| format!("Missing value for {}", arguments[index]))?;
        match arguments[index].as_str() {
            "--parent-pid" => {
                parent_pid = Some(
                    value
                        .parse::<u32>()
                        .map_err(|_| "Parent PID is invalid".to_string())?,
                )
            }
            "--source" => source = Some(PathBuf::from(value)),
            "--target" => target = Some(PathBuf::from(value)),
            "--manifest" => manifest = Some(PathBuf::from(value)),
            "--relaunch" => {
                relaunch = match value.as_str() {
                    "direct" => Relaunch::Direct,
                    "shell" => Relaunch::Shell,
                    _ => return Err(format!("Unknown relaunch mode: {value}")),
                }
            }
            option => return Err(format!("Unknown updater option: {option}")),
        }
        index += 2;
    }
    Ok(ApplyArguments {
        parent_pid: parent_pid.ok_or_else(|| "Missing --parent-pid".to_string())?,
        source: source.ok_or_else(|| "Missing --source".to_string())?,
        target: target.ok_or_else(|| "Missing --target".to_string())?,
        manifest: manifest.ok_or_else(|| "Missing --manifest".to_string())?,
        relaunch,
    })
}

fn absolute_path(path: &Path, label: &str) -> Result<PathBuf, String> {
    if !path.is_absolute() {
        return Err(format!("Update {label} path must be absolute"));
    }
    Ok(path.to_path_buf())
}

/// `path` resolved through links, so it can be compared with the running
/// executable.
fn canonical_path(path: &Path, label: &str) -> Result<PathBuf, String> {
    let path = absolute_path(path, label)?;
    fs::canonicalize(&path)
        .map(|path| simplify_verbatim(&path))
        .map_err(|error| format!("Update {label} {} is unavailable: {error}", path.display()))
}

/// `\\?\C:\dir\file` as `C:\dir\file`, which the desktop shell accepts.
/// Other verbatim forms (`\\?\UNC\...`) are kept as they are.
fn simplify_verbatim(path: &Path) -> PathBuf {
    let text = path.to_string_lossy();
    match text.strip_prefix(r"\\?\") {
        Some(rest)
            if rest.len() >= 3
                && rest.as_bytes()[0].is_ascii_alphabetic()
                && rest[1..].starts_with(":\\") =>
        {
            PathBuf::from(rest)
        }
        _ => path.to_path_buf(),
    }
}

/// `<target>.<suffix>` beside the installed executable.
fn sibling_path(target: &Path, suffix: &str) -> Result<PathBuf, String> {
    let name = target
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| "Installed Echo filename is invalid".to_string())?;
    Ok(target.with_file_name(format!("{name}.{suffix}")))
}

/// Read a user-supplied update file through a single handle that no other
/// process may write to or delete while it is open, refusing links and files
/// larger than `limit`. Everything the updater checks comes from these bytes,
/// so the path is never reopened.
fn read_update_file(path: &Path, limit: u64, label: &str) -> Result<Vec<u8>, String> {
    let path = absolute_path(path, label)?;
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_SHARE_READ: u32 = 0x0000_0001;
        const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
        options
            .share_mode(FILE_SHARE_READ)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    let file = options
        .open(&path)
        .map_err(|error| format!("Could not open the update {label}: {error}"))?;
    let metadata = file
        .metadata()
        .map_err(|error| format!("Could not inspect the update {label}: {error}"))?;
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;
        if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err(format!("The update {label} must not be a link"));
        }
    }
    if !metadata.is_file() {
        return Err(format!("The update {label} is not a file"));
    }
    if metadata.len() > limit {
        return Err(format!("The update {label} exceeds its size limit"));
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(limit.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|error| format!("Could not read the update {label}: {error}"))?;
    if bytes.len() as u64 > limit {
        return Err(format!("The update {label} exceeds its size limit"));
    }
    Ok(bytes)
}

/// Write the verified package to a fresh file in the install directory, so
/// the final install is a same-volume rename of bytes the updater wrote.
fn write_staged_payload(payload: &[u8], staged: &Path) -> Result<(), String> {
    if staged.exists() {
        fs::remove_file(staged)
            .map_err(|error| format!("Could not clear a previous staged update: {error}"))?;
    }
    let result = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(staged)
        .and_then(|mut file| {
            file.write_all(payload)?;
            file.sync_all()
        });
    if let Err(error) = result {
        let _ = fs::remove_file(staged);
        return Err(format!("Could not stage the update for install: {error}"));
    }
    Ok(())
}

fn wait_for_replaceable_target(
    target: &Path,
    backup: &Path,
    timeout: Duration,
) -> Result<(), String> {
    let started = Instant::now();
    loop {
        if backup.exists() {
            match fs::remove_file(backup) {
                Ok(()) => {}
                Err(_) if started.elapsed() < timeout => {
                    thread::sleep(Duration::from_millis(250));
                    continue;
                }
                Err(error) => return Err(format!("Could not clear rollback file: {error}")),
            }
        }
        match fs::rename(target, backup) {
            Ok(()) => return Ok(()),
            Err(_) if started.elapsed() < timeout => thread::sleep(Duration::from_millis(250)),
            Err(error) => {
                return Err(format!(
                    "Echo did not exit or its executable could not be replaced within {} seconds: {error}",
                    timeout.as_secs()
                ));
            }
        }
    }
}

fn install_staged(staged: &Path, target: &Path, expected: &str) -> Result<(), String> {
    fs::rename(staged, target).map_err(|error| format!("Could not install the update: {error}"))?;
    if sha256_file(target)? != expected {
        return Err("Installed executable failed its post-install SHA-256 check".to_string());
    }
    Ok(())
}

fn rollback(target: &Path, backup: &Path) -> Result<(), String> {
    if target.exists() {
        fs::remove_file(target)
            .map_err(|error| format!("Could not remove failed update: {error}"))?;
    }
    fs::rename(backup, target)
        .map_err(|error| format!("Could not restore previous Echo executable: {error}"))
}

fn sha256_hex(bytes: &[u8]) -> String {
    hex(digest::digest(&digest::SHA256, bytes).as_ref())
}

fn sha256_file(path: &Path) -> Result<String, String> {
    let mut file =
        File::open(path).map_err(|error| format!("Could not open {}: {error}", path.display()))?;
    let mut context = digest::Context::new(&digest::SHA256);
    let mut buffer = vec![0u8; 1024 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|error| format!("Could not hash {}: {error}", path.display()))?;
        if read == 0 {
            break;
        }
        context.update(&buffer[..read]);
    }
    Ok(hex(context.finish().as_ref()))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn append_log(path: &Path, line: &str) {
    if let Ok(mut log) = OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(log, "[{}] {line}", utc_timestamp(SystemTime::now()));
    }
}

/// `YYYY-MM-DDTHH:MM:SSZ` for a log line, without a date-time dependency.
fn utc_timestamp(time: SystemTime) -> String {
    let seconds = time
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs());
    let (days, second_of_day) = (seconds / 86_400, seconds % 86_400);
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let shifted = days as i64 + 719_468;
    let era = shifted.div_euclid(146_097);
    let day_of_era = shifted.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        second_of_day / 3_600,
        second_of_day / 60 % 60,
        second_of_day % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use ring::signature::KeyPair;

    #[test]
    fn log_timestamps_are_utc_dates() {
        assert_eq!(utc_timestamp(UNIX_EPOCH), "1970-01-01T00:00:00Z");
        let published = UNIX_EPOCH + Duration::from_secs(1_794_019_545);
        assert_eq!(utc_timestamp(published), "2026-11-07T02:45:45Z");
        let leap_day = UNIX_EPOCH + Duration::from_secs(1_709_208_000);
        assert_eq!(utc_timestamp(leap_day), "2024-02-29T12:00:00Z");
    }

    #[test]
    fn automatic_recheck_waits_for_its_interval_and_skips_busy_states() {
        let hour = Duration::from_secs(3_600);
        let interval = RECHECK_INTERVAL;
        assert!(!automatic_recheck_due(
            RecheckState::Settled,
            hour,
            interval
        ));
        assert!(automatic_recheck_due(
            RecheckState::Settled,
            interval,
            interval
        ));
        assert!(!automatic_recheck_due(
            RecheckState::Busy,
            10 * interval,
            interval
        ));
        assert!(!automatic_recheck_due(
            RecheckState::Failed,
            Duration::from_secs(60),
            interval
        ));
        assert!(automatic_recheck_due(
            RecheckState::Failed,
            RECHECK_AFTER_FAILURE,
            interval
        ));
        // A short test interval also shortens the failure retry.
        let short = Duration::from_secs(20);
        assert!(automatic_recheck_due(RecheckState::Failed, short, short));
    }

    fn arguments(extra: &[&str]) -> Vec<String> {
        [
            "--parent-pid",
            "1",
            "--source",
            "C:\\a.exe",
            "--target",
            "C:\\b.exe",
            "--manifest",
            "C:\\a.manifest.json",
        ]
        .iter()
        .chain(extra)
        .map(|value| value.to_string())
        .collect()
    }

    fn scratch_directory(name: &str) -> PathBuf {
        let directory =
            std::env::temp_dir().join(format!("echo-update-core-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&directory);
        fs::create_dir_all(&directory).unwrap();
        directory
    }

    fn payload_json(version: &str, platform: &str, sha256: &str) -> Vec<u8> {
        format!(
            r#"{{"schema_version":1,"channel":"stable","version":"{version}","published_at":"2026-09-25T00:00:00Z","changelog":"x","package":{{"platform":"{platform}","url":"https://echo-updates.revulate.dev/stable/x/echo.exe","bytes":10,"sha256":"{sha256}"}}}}"#
        )
        .into_bytes()
    }

    #[test]
    fn argument_errors_reach_the_fallback_log() {
        let log_path = fallback_log_path();
        assert!(log_path.ends_with(LOG_FILE_NAME));
        let marker = "--unit-test-unknown-option";
        let error = apply_update(&[marker.to_string(), "x".to_string()]).unwrap_err();
        assert!(error.contains(marker));
        let log = fs::read_to_string(&log_path).expect("fallback update log is written");
        assert!(log.contains(&format!("Update failed: Unknown updater option: {marker}")));
    }

    #[test]
    fn relaunch_mode_defaults_to_direct_and_parses_shell() {
        assert_eq!(
            parse_apply_arguments(&arguments(&[])).unwrap().relaunch,
            Relaunch::Direct
        );
        assert_eq!(
            parse_apply_arguments(&arguments(&["--relaunch", "shell"]))
                .unwrap()
                .relaunch,
            Relaunch::Shell
        );
        assert!(parse_apply_arguments(&arguments(&["--relaunch", "elevated"])).is_err());
    }

    #[test]
    fn command_line_hashes_are_not_accepted() {
        let error = parse_apply_arguments(&arguments(&["--sha256", &"a".repeat(64)]))
            .err()
            .unwrap();
        assert!(error.contains("--sha256"));
        let mut missing_manifest = arguments(&[]);
        missing_manifest.truncate(6);
        assert_eq!(
            parse_apply_arguments(&missing_manifest).err().as_deref(),
            Some("Missing --manifest")
        );
    }

    #[test]
    fn elevated_updates_must_relaunch_through_the_shell() {
        assert!(check_relaunch_mode(true, Relaunch::Direct).is_err());
        assert!(check_relaunch_mode(true, Relaunch::Shell).is_ok());
        assert!(check_relaunch_mode(false, Relaunch::Direct).is_ok());
    }

    #[test]
    fn update_targets_are_restricted_to_the_updaters_own_executable() {
        let installed = Path::new("C:\\Program Files\\Echo\\echo.exe");
        let relay = Path::new("C:\\Program Files\\Echo\\echo-updater.exe");
        let helper = Path::new("C:\\Users\\me\\Echo\\echo-self-updater.exe");
        let same = || Ok(true);
        let different = || Ok(false);

        assert_eq!(
            classify_stage(installed, installed, true, same),
            Ok(Stage::Installed)
        );
        assert!(classify_stage(installed, Path::new("C:\\Windows\\evil.exe"), true, same).is_err());
        assert!(
            classify_stage(
                installed,
                Path::new("C:\\Program Files\\Echo\\other.exe"),
                true,
                same
            )
            .is_err()
        );

        assert_eq!(
            classify_stage(relay, installed, true, same),
            Ok(Stage::Relay)
        );
        assert!(classify_stage(relay, installed, true, different).is_err());
        assert!(
            classify_stage(relay, Path::new("C:\\Windows\\System32\\x.exe"), true, same).is_err()
        );
        assert!(
            classify_stage(
                relay,
                Path::new("C:\\Program Files\\Echo\\echo-self-updater.exe"),
                true,
                same
            )
            .is_err()
        );
        assert!(
            classify_stage(
                relay,
                Path::new("C:\\Program Files\\Echo\\echo.dll"),
                true,
                same
            )
            .is_err()
        );

        assert_eq!(
            classify_stage(
                helper,
                Path::new("C:\\Users\\me\\Echo\\Echo.EXE"),
                false,
                same
            ),
            Ok(Stage::Helper)
        );
        assert!(classify_stage(helper, installed, false, same).is_err());

        let compatibility = Path::new("C:\\Tools\\echo-version.exe");
        assert_eq!(
            classify_stage(compatibility, installed, false, different),
            Ok(Stage::Compatibility)
        );
        assert!(classify_stage(compatibility, installed, true, same).is_err());
        let foreign = Path::new("C:\\Windows\\System32\\notepad.exe");
        assert!(classify_stage(installed, foreign, false, same).is_err());
        assert!(classify_stage(Path::new("C:\\Tools\\other.exe"), foreign, false, same).is_err());
    }

    #[test]
    fn signed_manifests_are_verified_and_must_be_newer() {
        let rng = ring::rand::SystemRandom::new();
        let pkcs8 = signature::Ed25519KeyPair::generate_pkcs8(&rng).unwrap();
        let key = signature::Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).unwrap();
        let sha = "AB".repeat(32);
        let payload = payload_json("99.0.0", platform_id(), &sha);
        let engine = base64::engine::general_purpose::STANDARD;
        let envelope = |payload: &[u8], signature: &[u8]| {
            format!(
                r#"{{"schema_version":1,"payload":"{}","signature":"{}"}}"#,
                engine.encode(payload),
                engine.encode(signature)
            )
            .into_bytes()
        };
        let signed = envelope(&payload, key.sign(&payload).as_ref());
        let verified =
            verify_envelope_payload_with_key(&signed, key.public_key().as_ref()).unwrap();
        assert_eq!(verified, payload);
        assert_eq!(
            verify_envelope_payload(&signed).err().as_deref(),
            Some(SIGNATURE_VERIFICATION_FAILED)
        );
        let tampered = payload_json("99.0.1", platform_id(), &sha);
        assert!(
            verify_envelope_payload_with_key(
                &envelope(&tampered, key.sign(&payload).as_ref()),
                key.public_key().as_ref()
            )
            .is_err()
        );

        let package = signed_package(&payload, "1.2.3").unwrap();
        assert_eq!(package.version, semver::Version::new(99, 0, 0));
        assert_eq!(package.bytes, 10);
        assert_eq!(package.sha256, "ab".repeat(32));
        assert!(signed_package(&payload, "99.0.0").is_err());
        assert!(signed_package(&payload, "100.0.0").is_err());
        assert!(signed_package(&payload_json("99.0.0", "other-os", &sha), "1.0.0").is_err());
        assert!(signed_package(&payload_json("99.0.0", platform_id(), "zz"), "1.0.0").is_err());
    }

    #[test]
    fn update_files_are_read_once_within_their_limit() {
        let directory = scratch_directory("read");
        let path = directory.join("package.exe");
        fs::write(&path, b"0123456789").unwrap();
        assert_eq!(
            read_update_file(&path, 10, "package").unwrap(),
            b"0123456789"
        );
        assert!(read_update_file(&path, 9, "package").is_err());
        assert!(read_update_file(Path::new("relative.exe"), 10, "package").is_err());
        assert!(read_update_file(&directory, 10, "package").is_err());
        let _ = fs::remove_dir_all(&directory);
    }

    #[test]
    fn staged_payload_is_installed_by_rename_and_rechecked() {
        let directory = scratch_directory("install");
        let target = directory.join("echo.exe");
        fs::write(&target, b"old").unwrap();
        let backup = sibling_path(&target, "previous").unwrap();
        let staged = sibling_path(&target, "new").unwrap();
        assert!(staged.ends_with("echo.exe.new"));

        write_staged_payload(b"new", &staged).unwrap();
        wait_for_replaceable_target(&target, &backup, Duration::ZERO).unwrap();
        install_staged(&staged, &target, &sha256_hex(b"new")).unwrap();
        assert_eq!(fs::read(&target).unwrap(), b"new");
        assert!(!staged.exists());

        write_staged_payload(b"bad", &staged).unwrap();
        fs::rename(&target, &backup).unwrap();
        assert!(install_staged(&staged, &target, &sha256_hex(b"new")).is_err());
        rollback(&target, &backup).unwrap();
        assert_eq!(fs::read(&target).unwrap(), b"new");
        let _ = fs::remove_dir_all(&directory);
    }

    #[test]
    fn verbatim_drive_paths_are_simplified() {
        assert_eq!(
            simplify_verbatim(Path::new(r"\\?\C:\Program Files\Echo\echo.exe")),
            PathBuf::from(r"C:\Program Files\Echo\echo.exe")
        );
        assert_eq!(
            simplify_verbatim(Path::new(r"\\?\UNC\server\share\echo.exe")),
            PathBuf::from(r"\\?\UNC\server\share\echo.exe")
        );
    }
}
