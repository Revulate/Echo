//! Bounded registry of media URLs already resolved to memory or disk.
//!
//! Load requests are deduplicated against this set so a URL that is already
//! resident never starts a second fetch. Entries accumulate over a session and
//! are only dropped on scattered error paths, so the set is hard-capped and
//! pruned arbitrarily when it fills.

use std::sync::LazyLock;

use dashmap::DashSet;

/// Hash a URL to a u64 key for allocation-free DashMap/DashSet lookups.
/// Uses ahash — same algorithm as `MemoryLayer::hash_url()`.
#[inline]
pub fn url_key(url: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = ahash::AHasher::default();
    url.hash(&mut hasher);
    hasher.finish()
}

/// URLs that have been successfully loaded
static LOADED_URLS: LazyLock<DashSet<u64>> = LazyLock::new(|| DashSet::with_capacity(5000));
/// Hard cap on LOADED_URLS. Inserts accumulate over a session and are only
/// removed on scattered error paths, so this set would otherwise grow without
/// bound on long-lived installs. When exceeded, ~10% of entries are pruned
/// arbitrarily. A pruned URL only re-permits a duplicate load attempt for it;
/// the disk layer is still consulted before any network fetch, so re-population
/// is a benign, self-healing hint refresh on the next request.
const LOADED_URLS_MAX: usize = 16_384;

/// Prune ~10% of LOADED_URLS entries when over cap.
/// Called just before inserting to keep size bounded.
#[inline]
fn maybe_prune_loaded_urls() {
    if LOADED_URLS.len() >= LOADED_URLS_MAX {
        let prune_count = LOADED_URLS_MAX / 10;
        let to_remove: Vec<u64> = LOADED_URLS.iter().take(prune_count).map(|r| *r).collect();
        for key in to_remove {
            LOADED_URLS.remove(&key);
        }
    }
}

/// Record that a URL key resolved successfully, pruning first when over cap.
#[inline]
pub fn mark_url_loaded(key: u64) {
    maybe_prune_loaded_urls();
    LOADED_URLS.insert(key);
}

/// Whether a URL key is already known to be loaded.
#[inline]
pub fn url_is_loaded(key: u64) -> bool {
    LOADED_URLS.contains(&key)
}

/// Drop a URL key so the next request may load it again.
#[inline]
pub fn forget_url(key: u64) {
    LOADED_URLS.remove(&key);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn is_url_loaded(url: &str) -> bool {
        url_is_loaded(url_key(url))
    }

    #[test]
    fn test_url_deduplication() {
        let url = "https://example.com/test.png".to_string();

        LOADED_URLS.insert(url_key(&url));
        assert!(is_url_loaded(&url));

        LOADED_URLS.remove(&url_key(&url));
        assert!(!is_url_loaded(&url));
    }
}
