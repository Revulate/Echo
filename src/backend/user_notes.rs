//! User notes storage and management.
//!
//! Provides persistent storage for per-user notes that appear in user cards.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{LazyLock, Mutex, OnceLock};

use super::lock_utils::MutexExt;

/// User note data
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct UserNote {
    pub username: String,
    pub note: String,
    pub updated_at: String,
}

/// Storage for user notes
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct UserNotesData {
    notes: HashMap<String, UserNote>,
}

struct UserNotesManager {
    file_path: PathBuf,
    legacy_path: PathBuf,
}

enum UserNotesRead {
    Missing,
    Ready(UserNotesData),
    Unreadable(String),
    Invalid(String),
}

impl UserNotesManager {
    fn new() -> Self {
        let config_dir = crate::benchmark_profile::config_root_or_dot();
        if let Err(e) = fs::create_dir_all(&config_dir) {
            tracing::warn!(
                "Failed to create config directory '{}': {}",
                config_dir.display(),
                e
            );
        }

        Self {
            file_path: config_dir.join("user_notes.json"),
            legacy_path: crate::benchmark_profile::legacy_cwd_file("user_notes.json")
                .unwrap_or_else(|| config_dir.join("user_notes.json")),
        }
    }

    fn backup_path(&self) -> PathBuf {
        self.file_path.with_extension("backup.json")
    }

    fn read(path: &PathBuf) -> UserNotesRead {
        let content = match fs::read_to_string(path) {
            Ok(content) => content,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return UserNotesRead::Missing;
            }
            Err(error) => {
                return UserNotesRead::Unreadable(format!(
                    "failed to read user notes '{}': {error}",
                    path.display()
                ));
            }
        };

        match serde_json::from_str(&content) {
            Ok(data) => UserNotesRead::Ready(data),
            Err(error) => UserNotesRead::Invalid(format!(
                "failed to parse user notes '{}': {error}",
                path.display()
            )),
        }
    }

    fn load(&self) -> Result<UserNotesData, String> {
        match Self::read(&self.file_path) {
            UserNotesRead::Ready(data) => Ok(data),
            UserNotesRead::Missing => self.load_after_missing_primary(),
            UserNotesRead::Invalid(primary_error) => self.load_backup(&primary_error),
            UserNotesRead::Unreadable(primary_error) => Err(primary_error),
        }
    }

    fn load_after_missing_primary(&self) -> Result<UserNotesData, String> {
        match Self::read(&self.backup_path()) {
            UserNotesRead::Ready(data) => {
                self.restore_primary(data, "the primary file was missing")
            }
            UserNotesRead::Missing => self.load_legacy_or_default(),
            UserNotesRead::Unreadable(error) | UserNotesRead::Invalid(error) => {
                Err(format!("the primary user-notes file was missing; {error}"))
            }
        }
    }

    fn load_backup(&self, primary_failure: &str) -> Result<UserNotesData, String> {
        match Self::read(&self.backup_path()) {
            UserNotesRead::Ready(data) => self.restore_primary(data, primary_failure),
            UserNotesRead::Missing => Err(format!(
                "{primary_failure}; no user-notes backup was available"
            )),
            UserNotesRead::Unreadable(error) | UserNotesRead::Invalid(error) => {
                Err(format!("{primary_failure}; {error}"))
            }
        }
    }

    fn load_legacy_or_default(&self) -> Result<UserNotesData, String> {
        match Self::read(&self.legacy_path) {
            UserNotesRead::Ready(data) => {
                tracing::info!(
                    "Migrating legacy user notes from '{}' to '{}'",
                    self.legacy_path.display(),
                    self.file_path.display()
                );
                if let Err(error) = self.save_primary(&data) {
                    tracing::warn!(
                        "Loaded legacy user notes but could not migrate them to '{}': {}",
                        self.file_path.display(),
                        error
                    );
                }
                Ok(data)
            }
            UserNotesRead::Missing => Ok(UserNotesData::default()),
            UserNotesRead::Unreadable(error) | UserNotesRead::Invalid(error) => Err(error),
        }
    }

    fn restore_primary(
        &self,
        data: UserNotesData,
        primary_failure: &str,
    ) -> Result<UserNotesData, String> {
        tracing::warn!(
            "Recovering user notes from '{}': {}",
            self.backup_path().display(),
            primary_failure
        );

        if let Err(error) = self.save_primary(&data) {
            tracing::warn!(
                "Loaded the user-notes backup but could not restore '{}': {}",
                self.file_path.display(),
                error
            );
        }

        Ok(data)
    }

    fn save_primary(&self, data: &UserNotesData) -> Result<(), String> {
        let content = serde_json::to_string_pretty(data).map_err(|e| e.to_string())?;
        crate::config::atomic_write_json(&self.file_path, &content).map_err(|e| e.to_string())?;
        Ok(())
    }

    fn save_backup(&self, data: &UserNotesData) -> Result<(), String> {
        let content = serde_json::to_string_pretty(data).map_err(|e| e.to_string())?;
        crate::config::atomic_write_json(&self.backup_path(), &content)
            .map_err(|e| e.to_string())?;
        Ok(())
    }
}

enum UserNotesState {
    Ready(UserNotesData),
    Unavailable(String),
}

static USER_NOTES: LazyLock<Mutex<UserNotesState>> = LazyLock::new(|| {
    let manager = UserNotesManager::new();
    let state = match manager.load() {
        Ok(data) => UserNotesState::Ready(data),
        Err(error) => {
            tracing::error!(
                "User notes are unavailable; refusing to overwrite them: {}",
                error
            );
            UserNotesState::Unavailable(error)
        }
    };
    Mutex::new(state)
});

static USER_NOTES_BACKUP_TX: OnceLock<crossbeam_channel::Sender<()>> = OnceLock::new();
static USER_NOTES_BACKUP_WARN_EMITTED: AtomicBool = AtomicBool::new(false);

fn user_notes_backup_tx() -> &'static crossbeam_channel::Sender<()> {
    USER_NOTES_BACKUP_TX.get_or_init(|| {
        let (tx, rx) = crossbeam_channel::bounded(1);
        if let Err(error) = std::thread::Builder::new()
            .name("user-notes-backup".into())
            .spawn(move || user_notes_backup_worker(rx))
        {
            tracing::warn!("Failed to spawn user-notes backup worker: {error}");
        }
        tx
    })
}

fn user_notes_backup_worker(rx: crossbeam_channel::Receiver<()>) {
    while rx.recv().is_ok() {
        let snapshot = {
            let state = USER_NOTES.lock_or_recover();
            match &*state {
                UserNotesState::Ready(data) => data.clone(),
                UserNotesState::Unavailable(_) => continue,
            }
        };

        let manager = UserNotesManager::new();
        if let Err(error) = manager.save_backup(&snapshot) {
            tracing::warn!(
                "Failed to refresh user-notes backup '{}': {}",
                manager.backup_path().display(),
                error
            );
        }
    }
}

fn schedule_user_notes_backup() {
    match user_notes_backup_tx().try_send(()) {
        Ok(()) | Err(crossbeam_channel::TrySendError::Full(())) => {}
        Err(crossbeam_channel::TrySendError::Disconnected(())) => {
            if !USER_NOTES_BACKUP_WARN_EMITTED.swap(true, Ordering::Relaxed) {
                tracing::warn!("User-notes backup worker unavailable");
            }
        }
    }
}

fn normalize_username(username: &str) -> String {
    username.trim().trim_start_matches('@').to_lowercase()
}

/// Get the full note entry for a user.
pub fn get_user_note_entry(username: &str) -> Option<UserNote> {
    let key = normalize_username(username);
    if key.is_empty() {
        return None;
    }

    let state = USER_NOTES.lock_or_recover();
    match &*state {
        UserNotesState::Ready(data) => data.notes.get(&key).cloned(),
        UserNotesState::Unavailable(_) => None,
    }
}

/// Set a note for a user
pub fn set_user_note(username: &str, note: String) -> Result<(), String> {
    let username_lower = normalize_username(username);
    if username_lower.is_empty() {
        return Err("Username is required for a user note".to_string());
    }
    let now = chrono::Utc::now().to_rfc3339();

    let manager = UserNotesManager::new();
    let mut state = USER_NOTES.lock_or_recover();
    let data = match &*state {
        UserNotesState::Ready(data) => data,
        UserNotesState::Unavailable(error) => {
            return Err(format!(
                "User notes could not be loaded, so this edit was not saved: {error}. Restart Echo after repairing the notes file"
            ));
        }
    };
    let mut updated = data.clone();
    updated.notes.insert(
        username_lower.clone(),
        UserNote {
            username: username_lower,
            note,
            updated_at: now,
        },
    );

    manager.save_primary(&updated)?;
    *state = UserNotesState::Ready(updated);
    drop(state);
    schedule_user_notes_backup();
    Ok(())
}

/// Delete a note for a user
pub fn delete_user_note(username: &str) -> Result<(), String> {
    let key = normalize_username(username);
    if key.is_empty() {
        return Err("Username is required for a user note".to_string());
    }

    let manager = UserNotesManager::new();
    let mut state = USER_NOTES.lock_or_recover();
    let data = match &*state {
        UserNotesState::Ready(data) => data,
        UserNotesState::Unavailable(error) => {
            return Err(format!(
                "User notes could not be loaded, so this edit was not saved: {error}. Restart Echo after repairing the notes file"
            ));
        }
    };
    let mut updated = data.clone();
    updated.notes.remove(&key);

    manager.save_primary(&updated)?;
    *state = UserNotesState::Ready(updated);
    drop(state);
    schedule_user_notes_backup();
    Ok(())
}
