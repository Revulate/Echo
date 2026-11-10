//! Observed Twitch username colors and explicit local color choices.
//!
//! The observed LRU is deliberately independent from overrides. Twitch user
//! IDs own overrides so a login rename or a recycled login cannot transfer a
//! local choice to another account.

use lru::LruCache;
use parking_lot::{Mutex, RwLock};
use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::collections::HashMap;
use std::num::NonZeroUsize;
use std::sync::LazyLock;
use std::sync::atomic::{AtomicU64, Ordering};

const USER_COLOR_CACHE_SIZE: usize = 1000;
const USER_COLOR_CACHE_FILE: &str = "user_colors.json";
const USER_COLOR_OVERRIDES_FILE: &str = "user_color_overrides.json";
const USER_COLOR_OVERRIDES_FILE_VERSION: u32 = 1;

/// Twitch's standard username palette. The order is part of the fallback
/// color assignment, so changing it recolors users.
const TWITCH_FALLBACK_USER_COLORS: [&str; 15] = [
    "#FF0000", "#0000FF", "#00FF00", "#B22222", "#FF7F50", "#9ACD32", "#FF4500", "#2E8B57",
    "#DAA520", "#D2691E", "#5F9EA0", "#1E90FF", "#FF69B4", "#8A2BE2", "#00FF7F",
];

/// User color LRU cache for @mention coloring.
static USER_COLOR_CACHE: LazyLock<Mutex<LruCache<String, String>>> = LazyLock::new(|| {
    Mutex::new(LruCache::new(
        NonZeroUsize::new(USER_COLOR_CACHE_SIZE).unwrap(),
    ))
});

/// Generation of the inputs behind `resolve_user_color`/`get_user_color`.
///
/// Bumped only when a stored color is actually added or changed, never on the
/// idempotent re-cache that every rendered message performs. Views that bake
/// resolved colors into a cached snapshot key on this so a color learned after
/// the snapshot was built is picked up.
static USER_COLOR_GENERATION: AtomicU64 = AtomicU64::new(0);

/// Explicit local username-color choices, kept separate from observed Twitch colors.
///
/// Twitch user IDs are authoritative so a rename cannot silently move an override to
/// another account. The login index is derived and only supports rows (mentions/replies)
/// that do not carry a user ID.
#[derive(Clone, Debug, Deserialize, Serialize)]
struct UserColorOverride {
    login: String,
    color: String,
}

#[derive(Default)]
struct UserColorOverrideStore {
    by_id: HashMap<String, UserColorOverride>,
    by_login: HashMap<String, String>,
}

static USER_COLOR_OVERRIDES: LazyLock<RwLock<UserColorOverrideStore>> =
    LazyLock::new(|| RwLock::new(UserColorOverrideStore::default()));

#[derive(Debug, Deserialize, Serialize)]
struct UserColorOverridesFile {
    version: u32,
    #[serde(default)]
    users: HashMap<String, UserColorOverride>,
}

fn get_user_color_cache_path() -> Option<std::path::PathBuf> {
    crate::storage_roots::config_root().map(|d| d.join(USER_COLOR_CACHE_FILE))
}

fn get_user_color_overrides_path() -> Option<std::path::PathBuf> {
    crate::storage_roots::config_root().map(|d| d.join(USER_COLOR_OVERRIDES_FILE))
}

fn normalize_user_color_login(login: &str) -> String {
    login.trim().trim_start_matches('@').to_ascii_lowercase()
}

pub(crate) fn normalize_user_color_hex(color: &str) -> Option<String> {
    let color = color.trim();
    let hex = color.strip_prefix('#').unwrap_or(color);
    if matches!(hex.len(), 3 | 6) && hex.chars().all(|ch| ch.is_ascii_hexdigit()) {
        Some(format!("#{hex}"))
    } else {
        None
    }
}

/// Load user colors from disk into the cache (call at app startup).
pub fn load_user_color_cache() {
    if let Some(path) = get_user_color_cache_path()
        && path.exists()
        && let Ok(content) = std::fs::read_to_string(&path)
        && let Ok(colors) = serde_json::from_str::<HashMap<String, String>>(&content)
    {
        {
            let mut cache = USER_COLOR_CACHE.lock();
            for (username, color) in colors {
                cache.put(username, color);
            }
            crate::log_cache!(info, "Loaded {} user colors from cache", cache.len());
        }
        bump_user_color_generation();
    }
}

/// Load explicit user color overrides from their versioned store.
///
/// The legacy `user_colors.json` file cannot be migrated: it contains both old command
/// writes and automatically observed colors, so promoting it would freeze ordinary Twitch
/// colors as overrides. Existing explicit choices from pre-v1 builds must be reissued once.
pub fn load_user_color_overrides() {
    let Some(path) = get_user_color_overrides_path() else {
        return;
    };
    if !path.exists() {
        return;
    }

    let result = (|| -> Result<UserColorOverrideStore, String> {
        let content = std::fs::read_to_string(&path).map_err(|error| error.to_string())?;
        let persisted: UserColorOverridesFile =
            serde_json::from_str(&content).map_err(|error| error.to_string())?;
        if persisted.version != USER_COLOR_OVERRIDES_FILE_VERSION {
            return Err(format!(
                "unsupported user color override version {} (expected {})",
                persisted.version, USER_COLOR_OVERRIDES_FILE_VERSION
            ));
        }

        let mut store = UserColorOverrideStore::default();
        for (user_id, mut entry) in persisted.users {
            let user_id = user_id.trim().to_string();
            entry.login = normalize_user_color_login(&entry.login);
            let Some(color) = normalize_user_color_hex(&entry.color) else {
                continue;
            };
            entry.color = color;
            if user_id.is_empty() {
                continue;
            }
            if !entry.login.is_empty() {
                store.by_login.insert(entry.login.clone(), user_id.clone());
            }
            store.by_id.insert(user_id, entry);
        }
        Ok(store)
    })();

    match result {
        Ok(store) => {
            let count = store.by_id.len();
            *USER_COLOR_OVERRIDES.write() = store;
            crate::log_cache!(info, "Loaded {} user color overrides", count);
        }
        Err(error) => crate::log_cache!(warn, "Failed to load user color overrides: {error}"),
    }
}

/// Save user colors from cache to disk (call periodically or on shutdown).
pub fn save_user_color_cache() {
    if let Some(path) = get_user_color_cache_path() {
        let colors: HashMap<String, String> = USER_COLOR_CACHE
            .lock()
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        match serde_json::to_string_pretty(&colors) {
            Ok(content) => match crate::config::atomic_write_json(&path, &content) {
                Ok(()) => crate::log_cache!(debug, "Saved {} user colors to cache", colors.len()),
                Err(error) => crate::log_persist!(
                    warn,
                    "Failed to save user color cache '{}': {}",
                    path.display(),
                    error
                ),
            },
            Err(error) => {
                crate::log_persist!(warn, "Failed to serialize user color cache: {}", error)
            }
        }
    }
}

/// Persist explicit user color overrides independently from observed Twitch colors.
pub fn save_user_color_overrides() -> Result<(), String> {
    let path = get_user_color_overrides_path()
        .ok_or_else(|| "config directory is unavailable".to_string())?;
    let persisted = {
        let store = USER_COLOR_OVERRIDES.read();
        UserColorOverridesFile {
            version: USER_COLOR_OVERRIDES_FILE_VERSION,
            users: store.by_id.clone(),
        }
    };
    let content = serde_json::to_string_pretty(&persisted).map_err(|error| error.to_string())?;
    crate::config::atomic_write_json(&path, &content).map_err(|error| error.to_string())?;
    crate::log_cache!(
        debug,
        "Saved {} user color overrides",
        persisted.users.len()
    );
    Ok(())
}

/// Current generation of the resolved user color inputs.
///
/// Monotonic; wraps only after 2^64 changes. Snapshot caches that bake user
/// colors in fold this into their cache key.
pub fn user_color_generation() -> u64 {
    USER_COLOR_GENERATION.load(Ordering::Acquire)
}

fn bump_user_color_generation() {
    USER_COLOR_GENERATION.fetch_add(1, Ordering::Release);
}

/// Cache a user's color for @mention rendering.
pub fn cache_user_color(username: &str, color: &str) {
    let username = username.to_lowercase();
    let mut cache = USER_COLOR_CACHE.lock();
    if cache
        .peek(&username)
        .is_none_or(|cached| cached.as_str() != color)
    {
        cache.put(username, color.to_string());
        drop(cache);
        bump_user_color_generation();
    } else {
        cache.promote(&username);
    }
}

/// Set an explicit user color override using Twitch user ID as the stable identity.
pub fn set_user_color_override(user_id: &str, login: &str, color: &str) -> Result<(), String> {
    let user_id = user_id.trim();
    let login = normalize_user_color_login(login);
    let Some(color) = normalize_user_color_hex(color) else {
        return Err("Color must be #RGB or #RRGGBB".to_string());
    };
    if user_id.is_empty() {
        return Err("Twitch user ID is required".to_string());
    }

    let mut store = USER_COLOR_OVERRIDES.write();
    if let Some(previous) = store.by_id.get(user_id) {
        let previous_login = previous.login.clone();
        if store.by_login.get(&previous_login).map(String::as_str) == Some(user_id) {
            store.by_login.remove(&previous_login);
        }
    }
    if !login.is_empty() {
        store.by_login.insert(login.clone(), user_id.to_string());
    }
    store
        .by_id
        .insert(user_id.to_string(), UserColorOverride { login, color });
    bump_user_color_generation();
    Ok(())
}

/// Clear only the explicit override, preserving the latest observed Twitch color.
pub fn remove_user_color_override(user_id: &str) -> bool {
    let mut store = USER_COLOR_OVERRIDES.write();
    let Some(previous) = store.by_id.remove(user_id.trim()) else {
        return false;
    };
    if !previous.login.is_empty()
        && store.by_login.get(&previous.login).map(String::as_str) == Some(user_id.trim())
    {
        store.by_login.remove(&previous.login);
    }
    bump_user_color_generation();
    true
}

/// Look up an explicit override by stable ID first, then normalized login.
pub fn get_user_color_override(user_id: Option<&str>, username: &str) -> Option<String> {
    let store = USER_COLOR_OVERRIDES.read();
    if let Some(user_id) = user_id.map(str::trim).filter(|id| !id.is_empty()) {
        return store.by_id.get(user_id).map(|entry| entry.color.clone());
    }
    let username = normalize_user_color_login(username);
    store
        .by_login
        .get(&username)
        .and_then(|user_id| store.by_id.get(user_id))
        .map(|entry| entry.color.clone())
}

/// Get a user's effective cached color for mention/reply rendering.
pub fn get_user_color(username: &str) -> Option<String> {
    if let Some(color) = get_user_color_override(None, username) {
        return Some(color);
    }
    USER_COLOR_CACHE
        .lock()
        .get(&normalize_user_color_login(username))
        .cloned()
}

fn fallback_user_color(user_id: &str) -> Option<&'static str> {
    let user_id = user_id.trim();
    if user_id.is_empty() {
        return None;
    }

    let seed = user_id.parse::<i32>().unwrap_or_else(|_| {
        user_id
            .chars()
            .map(|ch| ch.to_digit(10).map(|digit| digit as i32).unwrap_or(-1))
            .sum()
    });
    let index = seed.rem_euclid(TWITCH_FALLBACK_USER_COLORS.len() as i32) as usize;
    TWITCH_FALLBACK_USER_COLORS.get(index).copied()
}

/// Resolve the effective author color without allowing observed IRC data to replace user intent.
pub fn resolve_user_color<'a>(
    user_id: Option<&str>,
    username: &str,
    observed_color: Option<&'a str>,
) -> Option<Cow<'a, str>> {
    get_user_color_override(user_id, username)
        .map(Cow::Owned)
        .or_else(|| observed_color.map(Cow::Borrowed))
        .or_else(|| {
            if crate::backend::settings::get_colorize_users_without_color() {
                user_id.and_then(fallback_user_color).map(Cow::Borrowed)
            } else {
                None
            }
        })
        .or_else(|| {
            USER_COLOR_CACHE
                .lock()
                .get(&normalize_user_color_login(username))
                .cloned()
                .map(Cow::Owned)
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test;

    fn clear_stores() {
        USER_COLOR_CACHE.lock().clear();
        *USER_COLOR_OVERRIDES.write() = UserColorOverrideStore::default();
    }

    #[test]
    fn preserves_user_color_resolution_contract() {
        clear_stores();

        assert_eq!(normalize_user_color_login(" @MiXeD "), "mixed");
        assert_eq!(normalize_user_color_hex(" abc ").as_deref(), Some("#abc"));
        assert_eq!(
            normalize_user_color_hex("#A1b2C3").as_deref(),
            Some("#A1b2C3")
        );
        assert_eq!(normalize_user_color_hex("#12xz"), None);
        assert!(set_user_color_override("", "alice", "#123456").is_err());

        set_user_color_override("stable-one", "@Alice", "#abc").unwrap();
        assert_eq!(
            get_user_color_override(Some("stable-one"), "other").as_deref(),
            Some("#abc")
        );
        assert_eq!(get_user_color_override(Some("other-id"), "alice"), None);

        set_user_color_override("stable-one", "Bobby", "#111111").unwrap();
        assert_eq!(get_user_color_override(None, "alice"), None);
        set_user_color_override("stable-two", "bobby", "#222222").unwrap();
        assert_eq!(
            get_user_color_override(None, "BOBBY").as_deref(),
            Some("#222222")
        );
        assert_eq!(
            get_user_color_override(Some("stable-one"), "bobby").as_deref(),
            Some("#111111")
        );

        cache_user_color("Bobby", "#333333");
        assert_eq!(
            resolve_user_color(Some("stable-two"), "bobby", Some("#444444")).as_deref(),
            Some("#222222")
        );
        assert!(remove_user_color_override("stable-two"));
        assert_eq!(
            resolve_user_color(None, "bobby", Some("#444444")).as_deref(),
            Some("#444444")
        );
        assert_eq!(get_user_color("bobby").as_deref(), Some("#333333"));

        let before = user_color_generation();
        cache_user_color("generation-contract", "#555555");
        let changed = user_color_generation();
        cache_user_color("generation-contract", "#555555");
        assert!(changed > before);
        assert_eq!(user_color_generation(), changed);
        cache_user_color("generation-contract", "#666666");
        assert!(user_color_generation() > changed);

        assert_eq!(fallback_user_color(""), None);
        assert_eq!(fallback_user_color("0"), Some("#FF0000"));
        assert_eq!(fallback_user_color("1"), Some("#0000FF"));
        assert_eq!(fallback_user_color("2147483648"), Some("#00FF00"));
        clear_stores();
    }
}
