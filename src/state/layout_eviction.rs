//! Deferred hand-off of evicted message ids to the global layout cache.
//!
//! When a `MessageBuffer` drops old message ids (push overflow, capacity
//! shrink) or a channel closes, the layouts those ids left behind in
//! `GLOBAL_LAYOUT_CACHE` become permanently unreachable — the message is gone,
//! so no painter will ever request them again, yet the cache only frees them
//! once it hits capacity. Removing them eagerly would mean an O(map) scan per
//! evicted id on the message-ingest hot path.
//!
//! Instead, evicted ids are queued here (a cheap `Arc<str>` refcount bump under
//! a short-lived lock) and drained in one pass from the periodic maintenance
//! slot, which removes every queued id's layouts with a single map scan. This
//! module deliberately holds no dependency on the renderer so `MessageBuffer`
//! (and the tools that `#[path]`-include it) stay decoupled from the cache.

use parking_lot::Mutex;
use std::collections::HashSet;
use std::sync::Arc;
use std::sync::LazyLock;

/// Upper bound on the pending set so a stalled maintenance slot cannot let it
/// grow without limit. Overflow ids are simply not coupled-freed; the layout
/// cache's own capacity backstop still reclaims their layouts, so dropping them
/// here only forgoes the eager reclaim, never correctness.
const MAX_PENDING_IDS: usize = 20_000;

static PENDING_EVICTED_MESSAGE_IDS: LazyLock<Mutex<HashSet<Arc<str>>>> =
    LazyLock::new(|| Mutex::new(HashSet::new()));

/// Queue message ids whose cached layouts should be reclaimed on the next
/// maintenance pass. Every id's embed state is pruned even when the pending
/// set is full, and the pending lock is held only for the inserts.
pub fn queue_evicted_ids<I: IntoIterator<Item = Arc<str>>>(ids: I) {
    let ids: Vec<Arc<str>> = ids.into_iter().collect();
    for id in &ids {
        crate::backend::embeds::state::prune_message_id(id.as_ref());
    }
    let mut pending = PENDING_EVICTED_MESSAGE_IDS.lock();
    for id in ids {
        if pending.len() >= MAX_PENDING_IDS {
            break;
        }
        pending.insert(id);
    }
}

/// Queue a single evicted message id (convenience for the hot push path).
#[inline]
pub fn queue_evicted_id(id: Arc<str>) {
    crate::backend::embeds::state::prune_message_id(id.as_ref());
    let mut pending = PENDING_EVICTED_MESSAGE_IDS.lock();
    if pending.len() < MAX_PENDING_IDS {
        pending.insert(id);
    }
}

/// Take and clear the queued evicted ids for a single-pass reclaim.
pub fn drain_evicted_ids() -> HashSet<Arc<str>> {
    std::mem::take(&mut *PENDING_EVICTED_MESSAGE_IDS.lock())
}
