//! App-owned cache service facade.
//!
//! The loader still exposes compatibility functions for hot-path callers, but lifecycle,
//! metrics, reset, and shutdown entry points are centralized here.

use gpui::{Context, Task};

pub struct ImageCacheService {
    _runtime_task: Task<()>,
}

impl ImageCacheService {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let runtime_task = cx.spawn(async move |_, _| {
            let init_result = crate::backend::bridge::get_runtime()
                .spawn(async {
                    crate::cache::warming::load_priorities().await;
                    crate::cache::native_sizes::load().await;
                    super::loader::initialize().await
                })
                .await;

            match init_result {
                Ok(Ok(())) => {
                    let stats = super::loader::stats();
                    crate::log_cache!(
                        info,
                        "Cache initialized: memory={}MB, disk_entries={}",
                        stats.memory_used.load(std::sync::atomic::Ordering::Relaxed)
                            / (1024 * 1024),
                        stats
                            .disk_entries
                            .load(std::sync::atomic::Ordering::Relaxed)
                    );
                }
                Ok(Err(e)) => {
                    crate::log_cache!(warn, "Failed to initialize cache: {}", e);
                }
                Err(e) => {
                    crate::log_cache!(warn, "Cache initialization task failed: {e}");
                }
            }
        });

        Self {
            _runtime_task: runtime_task,
        }
    }

    pub async fn shutdown_runtime(reason: &'static str) {
        crate::log_cache!(info, reason, "ImageCacheService shutting down runtime");
        super::loader::shutdown().await;
    }
}
