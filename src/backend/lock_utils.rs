//! Lock Safety Utilities
//!
//! Provides safe wrappers around std::sync locks that recover from poisoning
//! instead of panicking. This prevents cascading failures when a thread panics
//! while holding a lock.
//!
//! ## Lock Acquisition Order
//!
//! To prevent deadlocks, locks must be acquired in this order:
//! 1. CHAT_MESSAGES (DashMap - per-channel sharding, minimal contention)
//! 2. MESSAGE_DEDUP (Mutex)
//! 3. USER_COLOR_CACHE (Mutex)
//! 4. RECENT_CHATTERS (RwLock)
//! 5. PENDING_OPTIMISTIC (Mutex)
//! 6. Other channel-specific locks
//!
//! Never acquire a lower-numbered lock while holding a higher-numbered one.

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
            tracing::debug!(
                "DashSetKeyGuard dropped, removed key '{}' from set",
                self.key
            );
        }
    }
}
