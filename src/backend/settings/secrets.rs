//! Credential secret storage and settings hydration.
//!
//! The parent settings module owns the settings model and persistence lifecycle.
//! This module owns only OS-keyring access, the profile-local fallback store,
//! and migration of runtime tokens between those storage boundaries.

use super::AppSettings;
use crate::backend::lock_utils::MutexExt;
use std::collections::HashMap;
use std::fs;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};

const KEYRING_SERVICE: &str = "echo_gpui";
const TOKEN_STORE_FILE: &str = "tokens.json";
const KEY_ACTIVE_TOKEN: &str = "active_access_token";
const KEY_ACTIVE_REFRESH_TOKEN: &str = "active_refresh_token";
static KEYRING_AVAILABLE: OnceLock<bool> = OnceLock::new();

fn account_token_key(username: &str) -> String {
    format!("account:{}:access_token", username.to_lowercase())
}

fn account_refresh_token_key(username: &str) -> String {
    format!("account:{}:refresh_token", username.to_lowercase())
}

fn cached_keyring_availability(cache: &OnceLock<bool>, probe: impl FnOnce() -> bool) -> bool {
    *cache.get_or_init(probe)
}

fn keyring_available() -> bool {
    cached_keyring_availability(&KEYRING_AVAILABLE, || {
        #[cfg(target_os = "windows")]
        {
            match windows_native_keyring_store::Store::new() {
                Ok(store) => keyring_core::set_default_store(store),
                Err(e) => tracing::warn!("Failed to initialize native credential store: {}", e),
            }
        }

        let available = keyring_core::get_default_store().is_some();
        if !available {
            tracing::warn!("OS keyring unavailable; using private local token store");
        }
        available
    })
}

fn keyring_entry(key: &str) -> Option<keyring_core::Entry> {
    if crate::storage_roots::is_isolated() {
        return None;
    }
    if !keyring_available() {
        return None;
    }
    match keyring_core::Entry::new(KEYRING_SERVICE, key) {
        Ok(entry) => Some(entry),
        Err(e) => {
            tracing::warn!("Failed to initialize keyring entry '{}': {}", key, e);
            None
        }
    }
}

fn token_store_path() -> PathBuf {
    crate::storage_roots::config_root_or_current_dir().join(TOKEN_STORE_FILE)
}

/// Serializes every read → (migrate) → write of the fallback token store, so
/// concurrent writers cannot drop each other's keys or race the migration.
static TOKEN_STORE_LOCK: Mutex<()> = Mutex::new(());

/// Set once an unloadable token store has been copied aside, so it can be
/// replaced by a fresh store instead of being quarantined on every read.
static TOKEN_STORE_PRESERVED: AtomicBool = AtomicBool::new(false);

/// Marks a token store sealed with Windows DPAPI (CurrentUser scope); the
/// rest of the file is base64 of the protected JSON.
#[cfg(windows)]
const DPAPI_STORE_PREFIX: &str = "echo-dpapi-v1:";

/// Seal the serialized store for disk. On Windows it is DPAPI-encrypted for
/// the current user; elsewhere it stays JSON (the file is mode 0600).
fn seal_token_store(json: &str) -> Result<String, String> {
    #[cfg(windows)]
    {
        use base64::Engine as _;
        let sealed = dpapi::protect(json.as_bytes())?;
        Ok(format!(
            "{DPAPI_STORE_PREFIX}{}",
            base64::engine::general_purpose::STANDARD.encode(sealed)
        ))
    }
    #[cfg(not(windows))]
    {
        Ok(json.to_string())
    }
}

/// Open a stored file: `(json, legacy)`, where `legacy` means a plaintext
/// store that should be rewritten sealed.
fn open_token_store(content: &str) -> Result<(String, bool), String> {
    #[cfg(windows)]
    {
        use base64::Engine as _;
        let Some(encoded) = content.trim().strip_prefix(DPAPI_STORE_PREFIX) else {
            return Ok((content.to_string(), true));
        };
        let sealed = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .map_err(|_| "sealed token store is not valid base64".to_string())?;
        // A transient DPAPI failure (profile still loading, service busy) must
        // not quarantine a good store, so one failure is retried.
        let json = dpapi::unprotect(&sealed).or_else(|error| {
            crate::log_persist!(debug, "Retrying token store unseal after: {error}");
            dpapi::unprotect(&sealed)
        })?;
        String::from_utf8(json)
            .map(|json| (json, false))
            .map_err(|_| "sealed token store is not UTF-8".to_string())
    }
    #[cfg(not(windows))]
    {
        Ok((content.to_string(), false))
    }
}

#[cfg(windows)]
mod dpapi {
    use windows::Win32::Foundation::{HLOCAL, LocalFree};
    use windows::Win32::Security::Cryptography::{
        CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData, CryptUnprotectData,
    };
    use windows::core::PCWSTR;

    fn blob(data: &[u8]) -> Result<CRYPT_INTEGER_BLOB, String> {
        Ok(CRYPT_INTEGER_BLOB {
            cbData: u32::try_from(data.len()).map_err(|_| "token store too large".to_string())?,
            pbData: data.as_ptr().cast_mut(),
        })
    }

    /// Copy out and free a DPAPI output blob.
    fn take(output: CRYPT_INTEGER_BLOB) -> Vec<u8> {
        if output.pbData.is_null() {
            return Vec::new();
        }
        // SAFETY: DPAPI returned `cbData` bytes at `pbData`, allocated with
        // LocalAlloc; they are copied before being freed exactly once.
        unsafe {
            let bytes = std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec();
            LocalFree(Some(HLOCAL(output.pbData.cast())));
            bytes
        }
    }

    pub(super) fn protect(data: &[u8]) -> Result<Vec<u8>, String> {
        let input = blob(data)?;
        let mut output = CRYPT_INTEGER_BLOB::default();
        // SAFETY: `input` borrows `data` for the call; DPAPI writes `output`.
        unsafe {
            CryptProtectData(
                &input,
                PCWSTR::null(),
                None,
                None,
                None,
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output,
            )
        }
        .map_err(|error| format!("DPAPI protect failed: {error}"))?;
        Ok(take(output))
    }

    pub(super) fn unprotect(data: &[u8]) -> Result<Vec<u8>, String> {
        let input = blob(data)?;
        let mut output = CRYPT_INTEGER_BLOB::default();
        // SAFETY: `input` borrows `data` for the call; DPAPI writes `output`.
        unsafe {
            CryptUnprotectData(
                &input,
                None,
                None,
                None,
                None,
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output,
            )
        }
        .map_err(|error| format!("DPAPI unprotect failed: {error}"))?;
        Ok(take(output))
    }
}

/// Parse a stored token file, migrating a legacy plaintext store to a sealed
/// one. The caller holds [`TOKEN_STORE_LOCK`].
fn parse_token_store(content: &str) -> Result<HashMap<String, String>, String> {
    let (json, legacy) = open_token_store(content)?;
    let tokens: HashMap<String, String> =
        serde_json::from_str(&json).map_err(|e| crate::utils::json_error_summary(&e))?;
    if legacy && cfg!(windows) && !tokens.is_empty() {
        if write_token_store(&tokens) {
            crate::log_persist!(
                info,
                "Migrated the plaintext token store to DPAPI encryption"
            );
        } else {
            crate::log_persist!(
                warn,
                "Could not migrate the plaintext token store to DPAPI encryption"
            );
        }
    }
    Ok(tokens)
}

/// The fallback token store. `Err` means the file exists but could not be
/// loaded or preserved, so writing it would erase every other stored token.
/// The caller holds [`TOKEN_STORE_LOCK`].
fn read_token_store() -> Result<HashMap<String, String>, String> {
    let path = token_store_path();
    let error = match fs::read_to_string(&path) {
        Ok(content) => match parse_token_store(&content) {
            Ok(tokens) => return Ok(tokens),
            Err(e) => e,
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(HashMap::new()),
        Err(e) => e.to_string(),
    };
    if TOKEN_STORE_PRESERVED.load(Ordering::Acquire) {
        return Ok(HashMap::new());
    }
    crate::log_persist!(
        error,
        "Token store '{}' is unloadable ({})",
        path.display(),
        error
    );
    if crate::utils::preserve_unloadable_file(&path, "token store") {
        TOKEN_STORE_PRESERVED.store(true, Ordering::Release);
        Ok(HashMap::new())
    } else {
        Err(error)
    }
}

/// Returns whether the store now holds `tokens` durably. The caller holds
/// [`TOKEN_STORE_LOCK`].
fn write_token_store(tokens: &HashMap<String, String>) -> bool {
    let path = token_store_path();
    if let Some(parent) = path.parent()
        && let Err(e) = fs::create_dir_all(parent)
    {
        tracing::warn!(
            "Failed to create token store directory '{}': {}",
            parent.display(),
            e
        );
        return false;
    }

    let content = match serde_json::to_string_pretty(tokens)
        .map_err(|e| format!("serialize: {e}"))
        .and_then(|json| seal_token_store(&json))
    {
        Ok(content) => content,
        Err(e) => {
            tracing::warn!("Failed to seal token store: {}", e);
            return false;
        }
    };
    let temp_path = path.with_extension("json.tmp");
    if let Err(e) = fs::write(&temp_path, content) {
        tracing::warn!(
            "Failed to write token store '{}': {}",
            temp_path.display(),
            e
        );
        return false;
    }
    #[cfg(unix)]
    if let Err(e) = fs::set_permissions(&temp_path, fs::Permissions::from_mode(0o600)) {
        tracing::warn!(
            "Failed to set token store permissions '{}': {}",
            temp_path.display(),
            e
        );
    }
    if let Err(e) = crate::utils::replace_file(&temp_path, &path) {
        tracing::warn!("Failed to replace token store '{}': {}", path.display(), e);
        let _ = fs::remove_file(&temp_path);
        return false;
    }
    true
}

/// Returns whether the secret is now stored in the fallback file.
fn token_store_set_secret(key: &str, value: &str) -> bool {
    if value.is_empty() {
        return true;
    }
    let _guard = TOKEN_STORE_LOCK.lock_or_recover();
    let Ok(mut tokens) = read_token_store() else {
        return false;
    };
    tokens.insert(key.to_string(), value.to_string());
    write_token_store(&tokens)
}

fn token_store_get_secret(key: &str) -> Option<String> {
    let _guard = TOKEN_STORE_LOCK.lock_or_recover();
    read_token_store()
        .ok()?
        .get(key)
        .cloned()
        .filter(|secret| !secret.trim().is_empty())
}

fn token_store_delete_secret(key: &str) {
    let _guard = TOKEN_STORE_LOCK.lock_or_recover();
    let Ok(mut tokens) = read_token_store() else {
        return;
    };
    if tokens.remove(key).is_some() {
        write_token_store(&tokens);
    }
}

/// Returns whether the secret is now stored in the keyring or the fallback file.
fn keyring_set_secret(key: &str, value: &str) -> bool {
    if value.is_empty() {
        return true;
    }
    match keyring_entry(key) {
        Some(entry) => match entry.set_password(value) {
            Err(e) => {
                tracing::warn!("Failed to store secret '{}' in keyring: {}", key, e);
                token_store_set_secret(key, value)
            }
            _ => {
                token_store_delete_secret(key);
                true
            }
        },
        None => token_store_set_secret(key, value),
    }
}

fn keyring_get_secret(key: &str) -> Option<String> {
    let Some(entry) = keyring_entry(key) else {
        return token_store_get_secret(key);
    };
    match entry.get_password() {
        Ok(secret) if !secret.trim().is_empty() => Some(secret),
        Ok(_) => token_store_get_secret(key),
        Err(keyring_core::Error::NoEntry) => token_store_get_secret(key),
        Err(e) => {
            tracing::warn!("Failed to read secret '{}' from keyring: {}", key, e);
            token_store_get_secret(key)
        }
    }
}

fn keyring_delete_secret(key: &str) {
    if let Some(entry) = keyring_entry(key) {
        match entry.delete_credential() {
            Ok(_) | Err(keyring_core::Error::NoEntry) => {}
            Err(e) => tracing::warn!("Failed to delete secret '{}' in keyring: {}", key, e),
        }
    }
    token_store_delete_secret(key);
}

pub(super) fn clear_account_secrets(username: &str) {
    // Held across the deletes so a concurrent save cannot record the old
    // secrets as stored in between.
    let mut persisted = PERSISTED_SECRETS.lock_or_recover();
    *persisted = None;
    keyring_delete_secret(&account_token_key(username));
    keyring_delete_secret(&account_refresh_token_key(username));
}

/// Fingerprint of the secrets the last fully successful
/// [`persist_tokens_to_keyring`] stored, so a settings save that changed no
/// secret skips the keyring and token-file writes. Cleared before a write and
/// by every other change to stored secrets; a failed write leaves it clear,
/// so the next save retries.
static PERSISTED_SECRETS: Mutex<Option<u64>> = Mutex::new(None);

fn secrets_fingerprint(settings: &AppSettings) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::hash::DefaultHasher::new();
    settings.token.hash(&mut hasher);
    settings.refresh_token.hash(&mut hasher);
    settings.accounts.len().hash(&mut hasher);
    for account in &settings.accounts {
        account.username.hash(&mut hasher);
        account.token.hash(&mut hasher);
        account.refresh_token.hash(&mut hasher);
    }
    hasher.finish()
}

/// Run `store` unless `persisted` says these exact secrets are already stored.
fn persist_unless_unchanged(
    persisted: &Mutex<Option<u64>>,
    settings: &AppSettings,
    store: impl FnOnce(&AppSettings) -> Result<(), Vec<String>>,
) -> Result<(), Vec<String>> {
    let fingerprint = secrets_fingerprint(settings);
    let mut persisted = persisted.lock_or_recover();
    if *persisted == Some(fingerprint) {
        return Ok(());
    }
    *persisted = None;
    let result = store(settings);
    if result.is_ok() {
        *persisted = Some(fingerprint);
    }
    result
}

/// Store every runtime token in the keyring (or the fallback file). Returns
/// the keys that could be stored in neither, so the caller can warn that
/// those sign-ins will not survive a restart. Skipped when the same secrets
/// were stored by the last call that fully succeeded.
pub(super) fn persist_tokens_to_keyring(settings: &AppSettings) -> Result<(), Vec<String>> {
    persist_unless_unchanged(&PERSISTED_SECRETS, settings, store_tokens)
}

fn store_tokens(settings: &AppSettings) -> Result<(), Vec<String>> {
    let mut failed = Vec::new();
    let mut store = |key: String, value: &str| {
        if !keyring_set_secret(&key, value) {
            failed.push(key);
        }
    };

    if let Some(token) = settings.token.as_deref() {
        store(KEY_ACTIVE_TOKEN.to_string(), token);
    } else {
        keyring_delete_secret(KEY_ACTIVE_TOKEN);
    }

    if let Some(refresh) = settings.refresh_token.as_deref() {
        store(KEY_ACTIVE_REFRESH_TOKEN.to_string(), refresh);
    } else {
        keyring_delete_secret(KEY_ACTIVE_REFRESH_TOKEN);
    }

    for account in &settings.accounts {
        if !account.token.is_empty() {
            store(account_token_key(&account.username), &account.token);
        }
        if let Some(refresh) = account.refresh_token.as_deref() {
            store(account_refresh_token_key(&account.username), refresh);
        }
    }

    if failed.is_empty() {
        Ok(())
    } else {
        Err(failed)
    }
}

pub(super) fn has_plaintext_runtime_tokens(settings: &AppSettings) -> bool {
    settings
        .token
        .as_deref()
        .is_some_and(|token| !token.trim().is_empty())
        || settings
            .refresh_token
            .as_deref()
            .is_some_and(|token| !token.trim().is_empty())
        || settings.accounts.iter().any(|account| {
            !account.token.trim().is_empty()
                || account
                    .refresh_token
                    .as_deref()
                    .is_some_and(|token| !token.trim().is_empty())
        })
}

pub(super) fn hydrate_tokens_from_keyring(settings: &mut AppSettings) {
    if crate::storage_roots::is_isolated() {
        return;
    }
    if let Some(token) = keyring_get_secret(KEY_ACTIVE_TOKEN) {
        settings.token = Some(token);
    }
    if let Some(refresh) = keyring_get_secret(KEY_ACTIVE_REFRESH_TOKEN) {
        settings.refresh_token = Some(refresh);
    }

    for account in &mut settings.accounts {
        if let Some(token) = keyring_get_secret(&account_token_key(&account.username)) {
            account.token = token;
        }
        if let Some(refresh) = keyring_get_secret(&account_refresh_token_key(&account.username)) {
            account.refresh_token = Some(refresh);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        cached_keyring_availability, open_token_store, persist_unless_unchanged, seal_token_store,
    };
    use core::prelude::v1::test;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Mutex, OnceLock};

    #[test]
    fn unchanged_secrets_are_stored_once_and_a_failure_is_retried() {
        let persisted = Mutex::new(None);
        let writes = AtomicUsize::new(0);
        let ok = |_: &super::AppSettings| {
            writes.fetch_add(1, Ordering::Relaxed);
            Ok(())
        };
        let mut settings = super::AppSettings::default();
        settings.token = Some("access".to_string());

        assert_eq!(persist_unless_unchanged(&persisted, &settings, ok), Ok(()));
        assert_eq!(persist_unless_unchanged(&persisted, &settings, ok), Ok(()));
        assert_eq!(writes.load(Ordering::Relaxed), 1);

        // A setting that is not a secret changes nothing.
        settings.font_scale += 0.5;
        assert_eq!(persist_unless_unchanged(&persisted, &settings, ok), Ok(()));
        assert_eq!(writes.load(Ordering::Relaxed), 1);

        // A failed write is retried by the next save with the same secrets.
        settings.refresh_token = Some("refresh".to_string());
        let failed = |_: &super::AppSettings| Err(vec!["active_refresh_token".to_string()]);
        assert!(persist_unless_unchanged(&persisted, &settings, failed).is_err());
        assert_eq!(persist_unless_unchanged(&persisted, &settings, ok), Ok(()));
        assert_eq!(writes.load(Ordering::Relaxed), 2);

        // Clearing the record (as an account removal does) forces a write.
        *persisted.lock().unwrap() = None;
        assert_eq!(persist_unless_unchanged(&persisted, &settings, ok), Ok(()));
        assert_eq!(writes.load(Ordering::Relaxed), 3);
    }

    #[test]
    fn every_secret_is_part_of_the_fingerprint() {
        use super::secrets_fingerprint;
        let mut settings = super::AppSettings::default();
        settings.accounts.push(super::super::Account {
            username: "viewer".to_string(),
            display_name: None,
            profile_image_url: None,
            token: "account".to_string(),
            refresh_token: None,
            client_id: String::new(),
            is_active: true,
            token_expires_at: None,
        });
        let base = secrets_fingerprint(&settings);
        let changed = |edit: &dyn Fn(&mut super::AppSettings)| {
            let mut copy = settings.clone();
            edit(&mut copy);
            secrets_fingerprint(&copy) != base
        };
        assert!(changed(&|s| s.token = Some("t".to_string())));
        assert!(changed(&|s| s.refresh_token = Some("r".to_string())));
        assert!(changed(&|s| s.accounts[0].token = "other".to_string()));
        assert!(changed(
            &|s| s.accounts[0].refresh_token = Some("r".to_string())
        ));
        assert!(changed(&|s| s.accounts[0].username = "renamed".to_string()));
        assert!(changed(&|s| s.accounts.clear()));
        assert!(!changed(&|s| s.font_scale += 0.5));
    }

    #[test]
    fn keyring_availability_probe_is_cached_when_unavailable() {
        let cache = OnceLock::new();
        let probes = AtomicUsize::new(0);

        assert!(!cached_keyring_availability(&cache, || {
            probes.fetch_add(1, Ordering::Relaxed);
            false
        }));
        assert!(!cached_keyring_availability(&cache, || {
            probes.fetch_add(1, Ordering::Relaxed);
            true
        }));
        assert_eq!(probes.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn keyring_availability_probe_is_cached_when_available() {
        let cache = OnceLock::new();
        let probes = AtomicUsize::new(0);

        assert!(cached_keyring_availability(&cache, || {
            probes.fetch_add(1, Ordering::Relaxed);
            true
        }));
        assert!(cached_keyring_availability(&cache, || {
            probes.fetch_add(1, Ordering::Relaxed);
            false
        }));
        assert_eq!(probes.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn token_store_seal_round_trips() {
        let json = r#"{"active_access_token":"abc"}"#;
        let sealed = seal_token_store(json).expect("seal");
        #[cfg(windows)]
        assert!(!sealed.contains("abc"));
        let (opened, legacy) = open_token_store(&sealed).expect("open");
        assert_eq!(opened, json);
        assert!(!legacy);
    }

    #[cfg(windows)]
    #[test]
    fn plaintext_token_store_is_legacy() {
        let (opened, legacy) = open_token_store("{}").expect("open");
        assert_eq!(opened, "{}");
        assert!(legacy);
    }
}
