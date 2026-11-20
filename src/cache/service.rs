//! Startup and shutdown of the media cache runtime.
//!
//! Backend initialization starts the loader (persisted warming priorities,
//! native sizes and the disk index load together) ahead of the first window;
//! everything else goes through `cache::loader`.

use gpui::Context;
use std::sync::atomic::{AtomicBool, Ordering};

static INITIALIZATION_STARTED: AtomicBool = AtomicBool::new(false);

/// Start initializing the media cache on the backend runtime, once. Disk
/// lookups made before the index is installed wait for it briefly, and index
/// writes wait until it is installed.
pub fn start_initialization() {
    if INITIALIZATION_STARTED.swap(true, Ordering::AcqRel) {
        return;
    }
    crate::backend::bridge::get_runtime().spawn(async {
        match super::loader::initialize().await {
            Ok(()) => {
                let stats = super::loader::stats();
                crate::log_cache!(
                    info,
                    "Cache initialized: memory={}MB, disk_entries={}",
                    stats.memory_used.load(std::sync::atomic::Ordering::Relaxed) / (1024 * 1024),
                    stats
                        .disk_entries
                        .load(std::sync::atomic::Ordering::Relaxed)
                );
            }
            Err(e) => {
                crate::log_cache!(warn, "Failed to initialize cache: {}", e);
            }
        }
    });
}

pub struct ImageCacheService;

impl ImageCacheService {
    pub fn new(_cx: &mut Context<Self>) -> Self {
        // Normally already started by backend initialization.
        start_initialization();
        Self
    }

    pub async fn shutdown_runtime(reason: &'static str) {
        crate::log_cache!(info, reason, "ImageCacheService shutting down runtime");
        super::loader::shutdown().await;
    }
}
