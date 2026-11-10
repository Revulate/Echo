//! Bridge API for Echo backend
//!
//! This module provides a high-level API for the GPUI frontend to interact
//! with the backend. It wraps the internal modules and provides type conversions.
//!
//! ## Module Structure
//! - `init`: Backend initialization and shutdown
//! - `chat`: IRC messaging, connection, message retrieval
//! - `emotes`: Emote loading, caching, categorization
//! - `auth`: Authentication state and login flows
//! - `settings`: All settings getters/setters
//! - `moderation`: Mod actions, bans, timeouts
//! - `events`: Polling, EventSub, embeds, metrics
//! - `chatterino_import`: Import from Chatterino preview, staging and backups
//! - `followed_live`: Followed channels that are live, for Live now lists
#![allow(clippy::type_complexity)]

mod auth;
mod chat;
mod emotes;
mod events;
mod init;
mod moderation;
mod settings;

pub use auth::*;
pub use chat::*;
pub use emotes::*;
pub use events::*;
pub use init::*;
pub use moderation::*;
pub use settings::*;

pub use super::chatterino_import;
pub use super::followed_live;
pub use super::types::*;

use dashmap::DashMap;
use std::sync::Arc;
use std::sync::LazyLock;
use tokio::runtime::{Builder, Runtime};

/// Global tokio runtime for async operations.
///
/// Keep the default worker counts bounded. `Runtime::new()` scales with CPU
/// count, which is unnecessary for chat IO and reserves avoidable stack memory
/// on high-core Windows machines.
pub(crate) static RUNTIME: LazyLock<Runtime> = LazyLock::new(|| {
    Builder::new_multi_thread()
        .worker_threads(crate::utils::env_usize("ECHO_BACKEND_THREADS", 4).clamp(1, 8))
        .max_blocking_threads(
            crate::utils::env_usize("ECHO_BACKEND_BLOCKING_THREADS", 12).clamp(2, 32),
        )
        .thread_name("echo-backend")
        .enable_all()
        .build()
        .unwrap_or_else(|error| {
            tracing::error!("Failed to create global backend runtime: {error}");
            std::process::abort();
        })
});

/// Get reference to global tokio runtime
/// Used by lifecycle manager and other components that need async execution
pub fn get_runtime() -> &'static Runtime {
    &RUNTIME
}

/// Safely run an async operation from any context (sync or async).
///
/// This is crucial for bridge functions that may be called from either:
/// - GPUI render callbacks (sync context - can use block_on directly)
/// - Other async tasks (async context - block_on would panic)
///
/// Detection strategy:
/// - If we're NOT in a tokio runtime, use block_on directly (safe)
/// - If we're IN a runtime, spawn on a separate thread to avoid nested runtime panic
///
/// Returns `Result<T, String>` to allow graceful handling of panics in the spawned thread.
pub(crate) fn block_on_safe<F, T>(future: F) -> Result<T, String>
where
    F: std::future::Future<Output = T> + Send + 'static,
    T: Send + 'static,
{
    if tokio::runtime::Handle::try_current().is_ok() {
        match std::thread::spawn(move || RUNTIME.block_on(future)).join() {
            Ok(result) => Ok(result),
            Err(panic_payload) => {
                let panic_msg = if let Some(s) = panic_payload.downcast_ref::<&str>() {
                    s.to_string()
                } else if let Some(s) = panic_payload.downcast_ref::<String>() {
                    s.clone()
                } else {
                    "Unknown panic".to_string()
                };
                tracing::error!("block_on_safe: background thread panicked: {}", panic_msg);
                Err(format!("Background task panicked: {}", panic_msg))
            }
        }
    } else {
        Ok(RUNTIME.block_on(future))
    }
}

/// Cached converted emote categories per channel
/// Key: channel_login (lowercase)
/// Generation-tagged category snapshot. The layout generation covers provider
/// visibility and picker display settings that do not advance emote versions.
pub(crate) struct CachedEmoteCategories {
    pub(crate) channel_version: u32,
    pub(crate) global_version: u32,
    pub(crate) user_version: u64,
    pub(crate) layout_version: u64,
    pub(crate) categories: Arc<Vec<EmoteCategory>>,
}

/// Uses Arc so returning cached data is O(1) instead of cloning thousands of emotes
pub(crate) static EMOTE_CATEGORY_CACHE: LazyLock<DashMap<String, CachedEmoteCategories>> =
    LazyLock::new(DashMap::new);

/// Clean up emote category cache for channels that are no longer active
/// Prevents unbounded memory growth when visiting many channels
pub fn cleanup_emote_category_cache(active_channels: &std::collections::HashSet<String>) {
    let before = EMOTE_CATEGORY_CACHE.len();
    EMOTE_CATEGORY_CACHE.retain(|channel, _| active_channels.contains(channel));
    let removed = before - EMOTE_CATEGORY_CACHE.len();
    if removed > 0 {
        tracing::debug!(
            removed = removed,
            remaining = EMOTE_CATEGORY_CACHE.len(),
            "Cleaned up emote category cache"
        );
    }
}
