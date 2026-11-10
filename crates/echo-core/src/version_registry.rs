//! Centralized version registry for polling-based state change detection.
//!
//! This module consolidates all version counters used throughout Echo into a single
//! source of truth with one counter type.
//!
//! ## Design Principles
//!
//! 1. **Single Source of Truth**: All version counters live here
//! 2. **Consistent Types**: All counters are AtomicU64
//! 3. **Memory Ordering**: Uses Acquire/Release pairs for proper synchronization
//! 4. **Linked Versions**: Some counters (e.g., emote versions) trigger related counters
//!
//! ## Usage Pattern
//!
//! Backend modules bump versions when state changes:
//! ```ignore
//! VersionRegistry::bump_chat_version();
//! ```
//!
//! UI components poll versions to detect changes:
//! ```ignore
//! if VersionRegistry::chat_version() != self.cached_version {
//!     // Refresh UI
//! }
//! ```

use dashmap::DashMap;
use std::sync::LazyLock;
use std::sync::atomic::{AtomicU64, Ordering};

// ============================================================================
// Version Counters
// ============================================================================

/// Chat message version - bumped when messages are added/removed in any channel.
/// Used by UI to detect when to refresh message lists.
static CHAT_VERSION: AtomicU64 = AtomicU64::new(0);

/// Emote resolution generation used in async layout request identity.
/// Split-local layout invalidation is driven by per-channel versions and message
/// snapshot replacement, not by a cross-channel resolver bump.
static EMOTE_RESOLUTION_VERSION: AtomicU64 = AtomicU64::new(0);

/// Global emote version - bumped when global emote set changes (BTTV, FFZ, 7TV global).
static GLOBAL_EMOTE_VERSION: AtomicU64 = AtomicU64::new(0);

/// User emote version - bumped when user's personal emotes change (7TV personal, subscriptions).
static USER_EMOTE_VERSION: AtomicU64 = AtomicU64::new(0);

/// Embed metadata version - bumped when URL embeds finish loading.
/// Uses debounced updates to prevent layout thrashing.
static EMBED_METADATA_VERSION: AtomicU64 = AtomicU64::new(0);

/// Auth state version - bumped when authentication state changes.
static AUTH_VERSION: AtomicU64 = AtomicU64::new(0);

/// Per-channel emote version tracking.
/// Key: lowercase channel login, Value: version counter.
/// Uses DashMap for lock-free concurrent access (prevents UI thread blocking).
static CHANNEL_EMOTE_VERSIONS: LazyLock<DashMap<String, u64>> = LazyLock::new(DashMap::new);

/// Highest version ever removed from `CHANNEL_EMOTE_VERSIONS`. A reopened
/// channel's first bump starts above it, so it never repeats a version that
/// stale caches from before the close may still hold.
static CHANNEL_EMOTE_VERSION_FLOOR: AtomicU64 = AtomicU64::new(0);

// ============================================================================
// VersionRegistry Implementation
// ============================================================================

/// Centralized version registry for all polling-based state change detection.
///
/// All methods are static - no instance needed.
pub struct VersionRegistry;

impl VersionRegistry {
    // === Chat Version ===

    /// Get the current chat message version.
    #[inline]
    pub fn chat_version() -> u64 {
        CHAT_VERSION.load(Ordering::Acquire)
    }

    /// Bump the chat version to signal messages have changed.
    /// Call after adding/removing messages from CHAT_MESSAGES.
    #[inline]
    pub fn bump_chat_version() {
        CHAT_VERSION.fetch_add(1, Ordering::Release);
    }

    // === Emote Resolution Version ===

    /// Get the current emote resolution generation for async layout request identity.
    #[inline]
    pub fn emote_resolution_version() -> u64 {
        EMOTE_RESOLUTION_VERSION.load(Ordering::Acquire)
    }

    // === Global Emote Version ===

    /// Get the current global emote version.
    #[inline]
    pub fn global_emote_version() -> u64 {
        GLOBAL_EMOTE_VERSION.load(Ordering::Acquire)
    }

    /// Bump the global emote version.
    /// ChatBackend refreshes each affected channel's message snapshot separately.
    pub fn bump_global_emote_version() {
        GLOBAL_EMOTE_VERSION.fetch_add(1, Ordering::Release);
    }

    // === User Emote Version ===

    /// Get the current user emote version.
    #[inline]
    pub fn user_emote_version() -> u64 {
        USER_EMOTE_VERSION.load(Ordering::Acquire)
    }

    /// Bump the user emote version.
    /// ChatBackend refreshes each affected channel's message snapshot separately.
    pub fn bump_user_emote_version() {
        USER_EMOTE_VERSION.fetch_add(1, Ordering::Release);
    }

    // === Channel Emote Version ===

    /// Get the current emote version for a specific channel.
    /// Uses DashMap for lock-free access - never blocks the UI thread.
    /// PERF: Avoids allocation when channel name is already lowercase (common case)
    #[inline]
    pub fn channel_emote_version(channel: &str) -> u64 {
        // Fast path: channel is already lowercase (Twitch channels always are)
        if channel.bytes().all(|b| !b.is_ascii_uppercase()) {
            CHANNEL_EMOTE_VERSIONS.get(channel).map(|v| *v).unwrap_or(0)
        } else {
            // Slow path: need to lowercase (rare, only for case-insensitive callers)
            CHANNEL_EMOTE_VERSIONS
                .get(&channel.to_lowercase())
                .map(|v| *v)
                .unwrap_or(0)
        }
    }

    /// Bump the emote version for a specific channel.
    /// This is the layout invalidation signal for that channel only.
    /// Uses DashMap for lock-free access - never blocks the UI thread.
    pub fn bump_channel_emote_version(channel: &str) {
        let key = channel.to_lowercase();
        // Use entry API for atomic read-modify-write
        CHANNEL_EMOTE_VERSIONS
            .entry(key)
            .and_modify(|v| *v += 1)
            .or_insert_with(|| CHANNEL_EMOTE_VERSION_FLOOR.load(Ordering::Acquire) + 1);
    }

    /// Forget a closed channel's emote version so the map does not grow for
    /// every channel ever opened.
    pub fn remove_channel_emote_version(channel: &str) {
        // Raise the floor while the shard lock is held so a concurrent bump
        // that re-inserts the key cannot reuse a pre-close version.
        CHANNEL_EMOTE_VERSIONS.remove_if(&channel.to_lowercase(), |_, version| {
            CHANNEL_EMOTE_VERSION_FLOOR.fetch_max(*version, Ordering::AcqRel);
            true
        });
    }

    // === Embed Metadata Version ===

    /// Get the current embed metadata version.
    #[inline]
    pub fn embed_metadata_version() -> u64 {
        EMBED_METADATA_VERSION.load(Ordering::Acquire)
    }

    /// Bump the embed metadata version.
    /// Note: For debounced updates, use schedule_embed_version_update() in bridge.rs instead.
    #[inline]
    pub fn bump_embed_metadata_version() {
        EMBED_METADATA_VERSION.fetch_add(1, Ordering::Release);
    }

    // === Auth Version ===

    /// Get the current auth state version.
    #[inline]
    pub fn auth_version() -> u64 {
        AUTH_VERSION.load(Ordering::Acquire)
    }

    /// Bump the auth version to signal auth state has changed.
    #[inline]
    pub fn bump_auth_version() {
        AUTH_VERSION.fetch_add(1, Ordering::Release);
    }

    // === Combined Versions ===
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_global_emote_does_not_bump_resolution() {
        // Global emote version bump intentionally does NOT cascade to resolution version.
        // Affected channels refresh their message snapshots independently.
        let res_before = VersionRegistry::emote_resolution_version();
        let global_before = VersionRegistry::global_emote_version();
        VersionRegistry::bump_global_emote_version();
        let res_after = VersionRegistry::emote_resolution_version();
        let global_after = VersionRegistry::global_emote_version();
        assert_eq!(
            res_after, res_before,
            "Resolution version should NOT change"
        );
        assert!(
            global_after > global_before,
            "Global emote version should change"
        );
    }

    #[test]
    fn test_channel_emote_version() {
        VersionRegistry::bump_channel_emote_version("test_channel");
        let v1 = VersionRegistry::channel_emote_version("test_channel");
        VersionRegistry::bump_channel_emote_version("test_channel");
        let v2 = VersionRegistry::channel_emote_version("test_channel");
        assert_eq!(v2, v1 + 1);
    }

    #[test]
    fn test_channel_version_case_insensitive() {
        VersionRegistry::bump_channel_emote_version("TestChannel");
        let v1 = VersionRegistry::channel_emote_version("testchannel");
        let v2 = VersionRegistry::channel_emote_version("TESTCHANNEL");
        assert_eq!(v1, v2);
    }

    #[test]
    fn removed_channel_version_never_repeats() {
        for _ in 0..3 {
            VersionRegistry::bump_channel_emote_version("closed_channel");
        }
        let before_close = VersionRegistry::channel_emote_version("closed_channel");
        VersionRegistry::remove_channel_emote_version("Closed_Channel");
        assert_eq!(VersionRegistry::channel_emote_version("closed_channel"), 0);
        VersionRegistry::bump_channel_emote_version("closed_channel");
        assert!(VersionRegistry::channel_emote_version("closed_channel") > before_close);
    }
}
