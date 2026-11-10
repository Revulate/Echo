//! Application path adapter for persistent cache warming priorities.

use echo_media_cache::warming::{WARMING_CACHE_FILE, WarmingTracker};
use std::sync::LazyLock;

static TRACKER: LazyLock<WarmingTracker> = LazyLock::new(|| {
    echo_media_cache::install_cache_log_enabled(|| crate::log::enabled("cache"));
    WarmingTracker::new(
        crate::benchmark_profile::config_root_or_current_dir().join(WARMING_CACHE_FILE),
    )
});

pub async fn load_priorities() {
    TRACKER.load_priorities().await;
}

pub async fn save_priorities() {
    TRACKER.save_priorities().await;
}

pub fn top_urls(limit: usize) -> Vec<String> {
    TRACKER.top_urls(limit)
}

pub fn record_access(url: &str, size_bytes: u32, is_animated: bool) {
    TRACKER.record_access(url, size_bytes, is_animated);
}
