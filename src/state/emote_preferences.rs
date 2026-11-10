//! Emote preferences persistence layer
//!
//! Handles storage and retrieval of:
//! - Favorite emotes (max 50)
//! - Recently used emotes (max 25)
//! - Usage frequency counts
//! - Collapsed category states
//!
//! Uses a global cached instance to avoid blocking the UI thread.
//! Call `init_preferences()` at app startup to load preferences in the background.

use parking_lot::RwLock;
use std::path::PathBuf;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

pub use echo_core::emote_preferences::{EmotePreferences, EmotePreferencesData, SAVE_DEBOUNCE_MS};

/// Global cached emote preferences, set once the background load finishes so
/// readers never block the UI thread on file I/O.
static PREFERENCES: OnceLock<RwLock<EmotePreferences>> = OnceLock::new();

enum PreferenceSaveRequest {
    Save,
    Flush(std::sync::mpsc::Sender<()>),
}

static PREFERENCE_SAVE_TX: OnceLock<crossbeam_channel::Sender<PreferenceSaveRequest>> =
    OnceLock::new();
static PREFERENCE_SAVE_WARN_EMITTED: AtomicBool = AtomicBool::new(false);

fn preference_save_tx() -> &'static crossbeam_channel::Sender<PreferenceSaveRequest> {
    PREFERENCE_SAVE_TX.get_or_init(|| {
        let (tx, rx) = crossbeam_channel::bounded(1);
        if let Err(error) = std::thread::Builder::new()
            .name("emote-preference-save".into())
            .spawn(move || preference_save_worker(rx))
        {
            crate::log_emotes!(
                warn,
                "Failed to spawn emote preference save worker: {error}"
            );
        }
        tx
    })
}

fn preference_save_worker(rx: crossbeam_channel::Receiver<PreferenceSaveRequest>) {
    while let Ok(request) = rx.recv() {
        match request {
            PreferenceSaveRequest::Save => {
                let mut deadline =
                    Instant::now() + std::time::Duration::from_millis(SAVE_DEBOUNCE_MS as u64);
                let mut flush_ack = None;
                let mut disconnected = false;

                loop {
                    let remaining = deadline.saturating_duration_since(Instant::now());
                    if remaining.is_zero() {
                        break;
                    }
                    match rx.recv_timeout(remaining) {
                        Ok(PreferenceSaveRequest::Save) => {
                            deadline = Instant::now()
                                + std::time::Duration::from_millis(SAVE_DEBOUNCE_MS as u64);
                        }
                        Ok(PreferenceSaveRequest::Flush(ack)) => {
                            flush_ack = Some(ack);
                            break;
                        }
                        Err(crossbeam_channel::RecvTimeoutError::Timeout) => break,
                        Err(crossbeam_channel::RecvTimeoutError::Disconnected) => {
                            disconnected = true;
                            break;
                        }
                    }
                }

                persist_pending_preferences();
                if let Some(ack) = flush_ack {
                    let _ = ack.send(());
                }
                if disconnected {
                    return;
                }
            }
            PreferenceSaveRequest::Flush(ack) => {
                persist_pending_preferences();
                let _ = ack.send(());
            }
        }
    }
}

fn schedule_preferences_save() {
    match preference_save_tx().try_send(PreferenceSaveRequest::Save) {
        Ok(()) | Err(crossbeam_channel::TrySendError::Full(_)) => {}
        Err(crossbeam_channel::TrySendError::Disconnected(_)) => {
            if !PREFERENCE_SAVE_WARN_EMITTED.swap(true, Ordering::Relaxed) {
                crate::log_emotes!(warn, "Emote preference save worker unavailable");
            }
        }
    }
}

/// Wait until the preference save worker has persisted all mutations queued before this call.
pub fn flush_preferences_saves(timeout: std::time::Duration) {
    let Some(tx) = PREFERENCE_SAVE_TX.get() else {
        return;
    };
    let started = Instant::now();
    let (ack_tx, ack_rx) = std::sync::mpsc::channel();
    if tx
        .send_timeout(PreferenceSaveRequest::Flush(ack_tx), timeout)
        .is_err()
    {
        crate::log_emotes!(warn, "Timed out queueing emote preference save flush");
        return;
    }

    let remaining = timeout.saturating_sub(started.elapsed());
    if remaining.is_zero() || ack_rx.recv_timeout(remaining).is_err() {
        crate::log_emotes!(warn, "Timed out waiting for emote preference save flush");
    }
}

/// Initialize preferences in background (call at app startup)
/// This loads preferences on the bounded blocking pool. Until loading completes,
/// get_preferences() returns a default instance to avoid blocking.
pub fn init_preferences() {
    crate::backend::bridge::get_runtime().spawn_blocking(|| {
        let prefs = load_preferences_sync();
        crate::log_emotes!(debug, "Emote preferences loaded from disk");

        let _ = PREFERENCES.set(RwLock::new(prefs));
        let _ = preference_save_tx();
        crate::log_emotes!(debug, "Emote preferences initialized");
    });
}

/// Get read access to the global preferences (non-blocking)
///
/// If preferences are still loading, returns a default empty instance.
/// This ensures the UI never blocks on file I/O.
pub fn get_preferences() -> PreferencesGuard {
    if let Some(prefs) = PREFERENCES.get() {
        return PreferencesGuard::Loaded(prefs.read());
    }
    PreferencesGuard::Default(EmotePreferences::new())
}

/// Get write access to the global preferences (non-blocking)
///
/// If preferences are still loading, returns None.
/// Callers should check and retry later if needed.
pub fn get_preferences_mut() -> Option<parking_lot::RwLockWriteGuard<'static, EmotePreferences>> {
    PREFERENCES.get().map(|p| p.write())
}

/// Guard that either holds a real lock or a default instance.
/// Note: The Default variant is larger (232 bytes vs 8 bytes for Loaded),
/// but boxing would add allocation overhead for this short-lived guard.
#[allow(clippy::large_enum_variant)]
pub enum PreferencesGuard {
    Loaded(parking_lot::RwLockReadGuard<'static, EmotePreferences>),
    Default(EmotePreferences),
}

impl std::ops::Deref for PreferencesGuard {
    type Target = EmotePreferences;

    fn deref(&self) -> &Self::Target {
        match self {
            PreferencesGuard::Loaded(guard) => guard,
            PreferencesGuard::Default(prefs) => prefs,
        }
    }
}

/// Load preferences from disk or create default (synchronous - for internal use)
///
/// IMPORTANT: This performs blocking file I/O. Use `get_preferences()` or
/// `get_preferences_mut()` instead to access the global cached instance.
fn load_preferences_sync() -> EmotePreferences {
    let path = get_prefs_path();
    match std::fs::read_to_string(&path) {
        Ok(json) => match serde_json::from_str::<EmotePreferencesData>(&json) {
            Ok(data) => EmotePreferences::from_data(data),
            Err(e) => {
                crate::log_emotes!(warn, "Failed to parse emote preferences: {}", e);
                EmotePreferences::new()
            }
        },
        Err(_) => EmotePreferences::new(),
    }
}

/// Install the storage hooks the GPUI-free preference model reaches through.
pub(crate) fn install_ports() {
    echo_core::emote_preferences::install_emote_preferences_ports(
        echo_core::emote_preferences::EmotePreferencesPorts {
            persist: persist_preferences_data,
            schedule_save: schedule_preferences_save,
        },
    );
}

fn persist_preferences_data(data: &EmotePreferencesData) -> Result<(), Box<dyn std::error::Error>> {
    let path = get_prefs_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_string_pretty(data)?;
    crate::config::atomic_write_json(&path, &json)
}

fn persist_pending_preferences() {
    let Some(preferences) = PREFERENCES.get() else {
        return;
    };
    let Some((revision, data)) = ({
        let preferences = preferences.read();
        preferences
            .is_dirty()
            .then(|| (preferences.revision(), preferences.data.clone()))
    }) else {
        return;
    };

    match persist_preferences_data(&data) {
        Ok(()) => {
            let mut preferences = preferences.write();
            if preferences.revision() == revision {
                preferences.mark_saved();
            }
            crate::log_emotes!(debug, "Saved emote preferences");
        }
        Err(error) => {
            crate::log_emotes!(error, "Failed to save emote preferences: {error}");
        }
    }
}

/// Get the path for emote preferences file
pub fn get_prefs_path() -> PathBuf {
    crate::storage_roots::emote_preferences_path()
}
