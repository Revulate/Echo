//! The isolated-profile receipt (`benchmark-profile-roots.json`) and the
//! hardened JSON receipt writer the live-channel probe shares with it.

use serde::Serialize;
use std::{
    env, fs,
    io::{self, Read, Write},
    path::{Component, Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use crate::storage_roots::ProfileRoots;

const RECEIPT_ENV: &str = "ECHO_BENCH_PROFILE_RECEIPT";
pub(super) const NONCE_ENV: &str = "ECHO_BENCH_OBSERVATION_NONCE";
const TARGET_COMMIT_ENV: &str = "ECHO_BENCH_TARGET_COMMIT";

static RECEIPT_TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

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

pub(super) fn write_profile_receipt(roots: &ProfileRoots) -> Result<(), String> {
    let path = echo_core::env::os_value(RECEIPT_ENV)
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
        observation_nonce: echo_core::env::value(NONCE_ENV),
        target_commit: echo_core::env::value(TARGET_COMMIT_ENV),
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

pub(super) fn reject_reparse_destination(path: &Path, name: &str) -> Result<(), String> {
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
pub(super) fn normalize_absolute_path(
    path: &Path,
    environment_name: &str,
) -> Result<PathBuf, String> {
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

pub(super) fn write_json_receipt(
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

pub(super) fn canonical_utc_timestamp(timestamp: chrono::DateTime<chrono::Utc>) -> String {
    format!(
        "{}.{:07}Z",
        timestamp.format("%Y-%m-%dT%H:%M:%S"),
        timestamp.timestamp_subsec_nanos() / 100
    )
}

pub(super) fn receipt_path(path: &Path) -> PathBuf {
    #[cfg(windows)]
    {
        if let Some(path) = path.to_str().and_then(|path| path.strip_prefix(r"\\?\")) {
            return PathBuf::from(path);
        }
    }
    path.to_path_buf()
}

pub(super) fn sha256_file(path: &Path) -> Result<String, String> {
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
