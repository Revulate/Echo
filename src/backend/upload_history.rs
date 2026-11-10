//! Bounded local history for successful uploads initiated by Echo.
//!
//! Deletion URLs may contain bearer-like secrets, so this history stays in a
//! dedicated local file and is never folded into portable settings exports.

use crate::backend::lock_utils::MutexExt;
use serde::{Deserialize, Serialize};
use std::fs;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{LazyLock, Mutex};

const HISTORY_FILE: &str = "upload_history.json";
const MAX_HISTORY_FILE_BYTES: u64 = 2 * 1024 * 1024;
pub const MAX_UPLOAD_HISTORY: usize = 100;
/// Reserved occurrence ID for links in local upload-result rows. This keeps
/// rich-preview suppression attached to the row after history eviction.
pub const UPLOAD_LINK_NO_PREVIEW: u32 = u32::MAX;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct UploadHistoryEntry {
    pub id: String,
    pub image_url: String,
    pub deletion_url: Option<String>,
    pub provider: String,
    pub file_name: String,
    pub mime_type: String,
    pub channel: String,
    pub uploaded_at_ms: i64,
}

/// Matched local upload metadata for link-preview policy.
///
/// This contains only the fields a renderer needs, rather than cloning the
/// entire persisted upload history for a single URL lookup.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KnownUploadUrl {
    /// True for the public share URL; false for the private deletion URL.
    pub is_share_url: bool,
    /// Direct image URL when Echo recorded an image MIME type.
    pub thumbnail_url: Option<String>,
}

static UPLOAD_HISTORY: LazyLock<Mutex<Vec<UploadHistoryEntry>>> =
    LazyLock::new(|| Mutex::new(load_history()));

pub fn entries() -> Vec<UploadHistoryEntry> {
    UPLOAD_HISTORY.lock_or_recover().clone()
}

pub fn len() -> usize {
    UPLOAD_HISTORY.lock_or_recover().len()
}

/// Look up a URL belonging to an Echo upload without cloning the full history.
///
/// Only share URLs recorded with an image MIME type expose a thumbnail source.
/// Rich-preview suppression is attached to the upload-result row's occurrence
/// instead of globally denying public share URLs, so a user can paste the same
/// URL into ordinary chat and receive the configured normal rich preview.
pub fn lookup_known_upload_url(url: &str) -> Option<KnownUploadUrl> {
    let url = url.trim();
    if url.is_empty() {
        return None;
    }

    let history = UPLOAD_HISTORY.lock_or_recover();
    history.iter().find_map(|entry| {
        if entry.image_url == url {
            return Some(KnownUploadUrl {
                is_share_url: true,
                thumbnail_url: is_image_mime_type(&entry.mime_type)
                    .then(|| super::image_uploader::upload_thumbnail_url(&entry.image_url)),
            });
        }

        (entry.deletion_url.as_deref() == Some(url)).then_some(KnownUploadUrl {
            is_share_url: false,
            thumbnail_url: None,
        })
    })
}

/// Whether a URL is an Echo upload deletion URL.
///
/// Deletion URLs may carry bearer-like secrets, so they must never enter
/// generic preview fetching even when pasted into an ordinary chat row.
#[inline]
pub fn is_known_upload_deletion_url(url: &str) -> bool {
    let url = url.trim();
    !url.is_empty()
        && UPLOAD_HISTORY
            .lock_or_recover()
            .iter()
            .any(|entry| entry.deletion_url.as_deref() == Some(url))
}

pub fn record_upload(
    image_url: String,
    deletion_url: Option<String>,
    file_name: &str,
    mime_type: &str,
    channel: &str,
) {
    let Ok(parsed) = url::Url::parse(&image_url) else {
        return;
    };
    if !matches!(parsed.scheme(), "http" | "https") {
        return;
    }

    let entry = UploadHistoryEntry {
        id: uuid::Uuid::new_v4().to_string(),
        provider: parsed.host_str().unwrap_or("unknown").to_string(),
        image_url,
        deletion_url: deletion_url.filter(|url| is_http_url(url)),
        file_name: bounded(file_name, 256),
        mime_type: bounded(mime_type, 128),
        channel: bounded(channel, 64),
        uploaded_at_ms: chrono::Utc::now().timestamp_millis(),
    };

    let snapshot = {
        let mut history = UPLOAD_HISTORY.lock_or_recover();
        history.insert(0, entry);
        history.truncate(MAX_UPLOAD_HISTORY);
        history.clone()
    };
    persist_history(&snapshot);
    wake_ui();
}

pub fn remove_entry(id: &str) -> bool {
    let (changed, snapshot) = {
        let mut history = UPLOAD_HISTORY.lock_or_recover();
        let before = history.len();
        history.retain(|entry| entry.id != id);
        (history.len() != before, history.clone())
    };
    if changed {
        persist_history(&snapshot);
        wake_ui();
    }
    changed
}

pub fn clear_history() -> usize {
    let removed = {
        let mut history = UPLOAD_HISTORY.lock_or_recover();
        let removed = history.len();
        history.clear();
        removed
    };
    if removed > 0 {
        persist_history(&[]);
        wake_ui();
    }
    removed
}

fn history_path() -> Option<PathBuf> {
    crate::storage_roots::config_root().map(|dir| dir.join(HISTORY_FILE))
}

/// Set when the history file failed to load and could not be copied aside;
/// persisting is refused until it can be, so deletion URLs are not erased.
static HISTORY_OVERWRITE_BLOCKED: AtomicBool = AtomicBool::new(false);

fn read_history_file(path: &Path) -> Result<Vec<UploadHistoryEntry>, String> {
    let metadata = match fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.to_string()),
    };
    if metadata.len() > MAX_HISTORY_FILE_BYTES {
        return Err(format!(
            "file is larger than {MAX_HISTORY_FILE_BYTES} bytes"
        ));
    }
    let contents = fs::read_to_string(path).map_err(|error| error.to_string())?;
    serde_json::from_str(&contents).map_err(|error| crate::utils::json_error_summary(&error))
}

fn load_history() -> Vec<UploadHistoryEntry> {
    let Some(path) = history_path() else {
        return Vec::new();
    };
    let mut history = match read_history_file(&path) {
        Ok(history) => history,
        Err(error) => {
            crate::log_persist!(
                error,
                "Upload history '{}' is unloadable ({}); starting empty",
                path.display(),
                error
            );
            if !crate::utils::preserve_unloadable_file(&path, "upload history") {
                HISTORY_OVERWRITE_BLOCKED.store(true, Ordering::Release);
            }
            return Vec::new();
        }
    };
    history.retain(|entry| {
        is_http_url(&entry.image_url) && entry.deletion_url.as_deref().is_none_or(is_http_url)
    });
    history.truncate(MAX_UPLOAD_HISTORY);
    history
}

fn persist_history(history: &[UploadHistoryEntry]) {
    let Some(path) = history_path() else {
        return;
    };
    if HISTORY_OVERWRITE_BLOCKED.load(Ordering::Acquire) {
        if !crate::utils::preserve_unloadable_file(&path, "upload history") {
            return;
        }
        HISTORY_OVERWRITE_BLOCKED.store(false, Ordering::Release);
    }
    let Ok(contents) = serde_json::to_string_pretty(history) else {
        return;
    };
    let Some(parent) = path.parent() else {
        return;
    };
    if let Err(error) = fs::create_dir_all(parent) {
        crate::log_persist!(warn, path = %parent.display(), %error, "Could not create upload history directory");
        return;
    }

    let temporary = path.with_extension("json.tmp");
    if let Err(error) = fs::write(&temporary, contents) {
        crate::log_persist!(warn, path = %temporary.display(), %error, "Could not write upload history");
        return;
    }
    #[cfg(unix)]
    if let Err(error) = fs::set_permissions(&temporary, fs::Permissions::from_mode(0o600)) {
        crate::log_persist!(warn, path = %temporary.display(), %error, "Could not restrict upload history permissions");
        let _ = fs::remove_file(&temporary);
        return;
    }
    if let Err(error) = crate::utils::replace_file(&temporary, &path) {
        crate::log_persist!(warn, path = %path.display(), %error, "Could not publish upload history");
        let _ = fs::remove_file(&temporary);
    }
}

fn is_http_url(value: &str) -> bool {
    url::Url::parse(value)
        .ok()
        .is_some_and(|url| matches!(url.scheme(), "http" | "https"))
}

#[inline]
fn is_image_mime_type(value: &str) -> bool {
    value.split(';').next().is_some_and(|mime| {
        let mime = mime.trim();
        mime.as_bytes()
            .get(..6)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case(b"image/"))
    })
}

fn bounded(value: &str, max_chars: usize) -> String {
    value.trim().chars().take(max_chars).collect()
}

fn wake_ui() {
    crate::managers::frame_rate_controller::record_activity();
    crate::managers::frame_rate_controller::wakeup();
}
