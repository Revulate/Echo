//! Persistent media cache runtime and GPUI `RenderImage` decoding.
//!
//! Disk persistence and warming policy remain UI-independent; the decode module
//! intentionally depends on GPUI to construct the application's render images.

use std::sync::OnceLock;

pub mod decode;
pub mod disk;
pub mod frames;
pub mod memory;
pub mod registry;
pub mod warming;

static CACHE_LOG_ENABLED: OnceLock<fn() -> bool> = OnceLock::new();

pub fn install_cache_log_enabled(callback: fn() -> bool) {
    let _ = CACHE_LOG_ENABLED.set(callback);
}

pub fn cache_log_enabled() -> bool {
    CACHE_LOG_ENABLED.get().is_none_or(|callback| callback())
}

#[macro_export]
macro_rules! log_cache {
    (trace, $($arg:tt)*) => {{
        if $crate::cache_log_enabled() { tracing::trace!(target: "echo::cache", $($arg)*) }
    }};
    (debug, $($arg:tt)*) => {{
        if $crate::cache_log_enabled() { tracing::debug!(target: "echo::cache", $($arg)*) }
    }};
    (info, $($arg:tt)*) => {{
        if $crate::cache_log_enabled() { tracing::info!(target: "echo::cache", $($arg)*) }
    }};
    (warn, $($arg:tt)*) => {{ tracing::warn!(target: "echo::cache", $($arg)*) }};
    (error, $($arg:tt)*) => {{ tracing::error!(target: "echo::cache", $($arg)*) }};
}
