//! Lock helpers shared by the backend.
//!
//! `MutexExt`/`RwLockExt` recover a poisoned std lock instead of panicking, so
//! one panicking thread does not take every later lock user down with it.
//!
//! ## Lock ordering
//!
//! `blocked_users::visibility_commit_guard()` is taken before the blocked-user
//! cache and before the per-channel chat buffers it publishes into, and is never
//! held across network or parsing work. `CHAT_MESSAGES`, `MESSAGE_DEDUP` and
//! `RECENT_CHATTERS` are sharded `DashMap`s: never hold a reference into one
//! entry while writing another key of the same map.

pub use echo_core::sync::{MutexExt, RwLockExt};

/// RAII guard that removes a key from a DashSet<String> when dropped.
/// This ensures the key is removed even if the code panics.
///
/// # Example
/// ```ignore
/// static REFRESHING: LazyLock<DashSet<String>> = ...;
/// let _guard = DashSetKeyGuard::new(&REFRESHING, "channel_name".to_string());
/// // ... do work ...
/// // _guard drops here, removing "channel_name" from the set
/// ```
pub struct DashSetKeyGuard<'a> {
    set: &'a dashmap::DashSet<String>,
    key: String,
    removed: bool,
}

impl<'a> DashSetKeyGuard<'a> {
    /// Create a new guard. The key is NOT automatically inserted -
    /// you should insert it before creating the guard.
    pub fn new(set: &'a dashmap::DashSet<String>, key: String) -> Self {
        Self {
            set,
            key,
            removed: false,
        }
    }

    /// Manually remove the key (prevents removal on drop).
    pub fn remove_now(&mut self) {
        if !self.removed {
            self.set.remove(&self.key);
            self.removed = true;
        }
    }
}

impl Drop for DashSetKeyGuard<'_> {
    fn drop(&mut self) {
        if !self.removed {
            self.set.remove(&self.key);
            crate::log_app!(
                debug,
                "DashSetKeyGuard dropped, removed key '{}' from set",
                self.key
            );
        }
    }
}
