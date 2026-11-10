//! Other chatters' 7TV personal emotes.
//!
//! A sender's 7TV personal emote set applies to that sender's messages.
//! The set id comes from 7TV `entitlement.create` events for open
//! channels, or from the 7TV user profile the cosmetics cache fetches for
//! visible senders; the set itself is fetched here
//! once, with bounded concurrency, and kept in a bounded LRU. When a set
//! arrives, that sender's resident rows are re-tokenized so the emotes show.

use super::{EmoteCache, EmoteNameMap, PERSONAL_SEVENTV_CHANNEL, SevenTvExactSetFetchOutcome};
use dashmap::DashMap;
use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock};
use std::time::{Duration, Instant};
use tokio::sync::Semaphore;

/// Sender -> personal set id, bounded like the cosmetics user cache.
const USERS_MAX: usize = 2_000;
/// Loaded personal sets. Each holds at most a handful of emotes.
const SETS_MAX: usize = 256;
const SET_FETCH_CONCURRENCY: usize = 4;
const FAILED_SET_RETRY: Duration = Duration::from_secs(10 * 60);
/// Minimum gap between fetches of one set, so an evicted set that is still in
/// use reloads without refetching in a loop when more sets are active than fit.
const SET_REFETCH_INTERVAL: Duration = Duration::from_secs(60);
/// Senders re-tokenized when one in-flight set arrives.
const WAITING_USERS_MAX: usize = 64;

static USER_SETS: LazyLock<DashMap<Arc<str>, UserSet>> = LazyLock::new(DashMap::new);
static SETS: LazyLock<Mutex<HashMap<Arc<str>, LoadedSet>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
type WaitingSenders = HashMap<Arc<str>, Vec<Arc<str>>>;
/// In-flight set id -> senders waiting for it.
static IN_FLIGHT: LazyLock<Mutex<WaitingSenders>> = LazyLock::new(|| Mutex::new(HashMap::new()));
static FAILED_SETS: LazyLock<DashMap<Arc<str>, Instant>> = LazyLock::new(DashMap::new);
static LAST_FETCH: LazyLock<DashMap<Arc<str>, Instant>> = LazyLock::new(DashMap::new);
static SET_FETCH_PERMITS: LazyLock<Semaphore> =
    LazyLock::new(|| Semaphore::new(SET_FETCH_CONCURRENCY));

struct UserSet {
    set_id: Arc<str>,
    /// Channels the sender was seen in, for re-tokenizing their rows.
    channels: Vec<Arc<str>>,
    last_seen_ms: AtomicU64,
}

struct LoadedSet {
    lookup: Arc<EmoteNameMap>,
    last_used: Instant,
}

/// The personal emotes of the Twitch user `user_id`, if their set is loaded.
/// A known set that was evicted is fetched again, and the sender's rows are
/// re-tokenized when it arrives.
pub fn lookup_for_user(user_id: &str) -> Option<Arc<EmoteNameMap>> {
    let (user_key, set_id) = {
        let entry = USER_SETS.get(user_id)?;
        entry
            .last_seen_ms
            .store(crate::utils::unix_now_ms(), Ordering::Relaxed);
        (Arc::clone(entry.key()), Arc::clone(&entry.set_id))
    };
    if let Some(set) = SETS.lock().get_mut(&set_id) {
        set.last_used = Instant::now();
        return Some(Arc::clone(&set.lookup));
    }
    request_set(set_id, user_key);
    None
}

/// Record `user_id`'s personal set from their 7TV profile, loading it when new.
/// `channels` are where the user was seen, for re-tokenizing their rows.
pub fn note_user_personal_set(user_id: &str, set_id: Option<&str>, channels: Vec<Arc<str>>) {
    let Some(set_id) = set_id.filter(|id| !id.is_empty()) else {
        if USER_SETS.remove(user_id).is_some() {
            retokenize_in_background(vec![(Arc::from(user_id), channels)]);
        }
        return;
    };
    let set_id: Arc<str> = Arc::from(set_id);
    let user_id: Arc<str> = Arc::from(user_id);
    let previous = USER_SETS.insert(
        Arc::clone(&user_id),
        UserSet {
            set_id: Arc::clone(&set_id),
            channels: channels.clone(),
            last_seen_ms: AtomicU64::new(crate::utils::unix_now_ms()),
        },
    );
    if USER_SETS.len() > USERS_MAX {
        prune_users();
    }
    let changed = previous.is_none_or(|previous| previous.set_id != set_id);
    if SETS.lock().contains_key(&set_id) {
        if changed {
            retokenize_in_background(vec![(user_id, channels)]);
        }
        return;
    }
    request_set(set_id, user_id);
}

/// Record a personal set pushed by a 7TV `entitlement.create` event.
/// `channels` are the open channels the entitlement could come from; a sender
/// already known keeps the channels its profile was seen in.
pub fn note_entitled_personal_set(user_id: &str, set_id: &str, channels: Vec<Arc<str>>) {
    let known_channels = match USER_SETS.get(user_id) {
        Some(entry) if entry.set_id.as_ref() == set_id => {
            entry
                .last_seen_ms
                .store(crate::utils::unix_now_ms(), Ordering::Relaxed);
            return;
        }
        Some(entry) => Some(entry.channels.clone()),
        None => None,
    };
    note_user_personal_set(user_id, Some(set_id), known_channels.unwrap_or(channels));
}

/// Drop a personal set removed by a 7TV `entitlement.delete` event, if it is
/// still the sender's current one.
pub fn forget_entitled_personal_set(user_id: &str, set_id: &str) {
    if let Some((user_id, entry)) =
        USER_SETS.remove_if(user_id, |_, entry| entry.set_id.as_ref() == set_id)
    {
        retokenize_in_background(vec![(user_id, entry.channels)]);
    }
}

fn request_set(set_id: Arc<str>, user_id: Arc<str>) {
    if FAILED_SETS
        .get(&set_id)
        .is_some_and(|failed_at| failed_at.elapsed() < FAILED_SET_RETRY)
    {
        return;
    }
    {
        let mut in_flight = IN_FLIGHT.lock();
        if let Some(waiting) = in_flight.get_mut(&set_id) {
            if waiting.len() < WAITING_USERS_MAX && !waiting.contains(&user_id) {
                waiting.push(user_id);
            }
            return;
        }
        if LAST_FETCH
            .get(&set_id)
            .is_some_and(|fetched_at| fetched_at.elapsed() < SET_REFETCH_INTERVAL)
        {
            return;
        }
        in_flight.insert(Arc::clone(&set_id), vec![user_id]);
    }
    LAST_FETCH.insert(Arc::clone(&set_id), Instant::now());
    crate::backend::bridge::get_runtime().spawn(async move {
        let loaded = {
            let _permit = SET_FETCH_PERMITS
                .acquire()
                .await
                .expect("personal emote set semaphore is never closed");
            super::fetch_7tv_emote_set_by_id(&set_id, Some(PERSONAL_SEVENTV_CHANNEL)).await
        };
        let emotes = match loaded {
            Ok(SevenTvExactSetFetchOutcome::Present { emotes, .. }) => emotes,
            Ok(SevenTvExactSetFetchOutcome::TerminallyAbsent { .. }) => Vec::new(),
            Err(error) => {
                FAILED_SETS.insert(Arc::clone(&set_id), Instant::now());
                IN_FLIGHT.lock().remove(&set_id);
                crate::log_emotes!(debug, "7TV personal set {} failed: {}", set_id, error);
                return;
            }
        };
        let mut lookup = EmoteNameMap::with_capacity(emotes.len() * 2);
        for emote in emotes {
            EmoteCache::insert_emote_name_keys(&mut lookup, emote);
        }
        store_set(Arc::clone(&set_id), lookup);
        let waiting = IN_FLIGHT.lock().remove(&set_id).unwrap_or_default();
        let users = waiting
            .into_iter()
            .filter_map(|user_id| {
                let channels = USER_SETS
                    .get(&user_id)
                    .filter(|entry| entry.set_id == set_id)
                    .map(|entry| entry.channels.clone())?;
                Some((user_id, channels))
            })
            .collect();
        retokenize_in_background(users);
    });
}

/// Re-tokenizing scans and reparses channel buffers, so it runs off the async
/// workers.
fn retokenize_in_background(users: Vec<(Arc<str>, Vec<Arc<str>>)>) {
    if users.is_empty() {
        return;
    }
    crate::backend::bridge::get_runtime().spawn_blocking(move || {
        for (user_id, channels) in users {
            crate::backend::irc::retokenize_user_messages(&user_id, &channels);
        }
    });
}

fn store_set(set_id: Arc<str>, lookup: EmoteNameMap) {
    let mut sets = SETS.lock();
    if sets.len() >= SETS_MAX
        && !sets.contains_key(&set_id)
        && let Some(oldest) = sets
            .iter()
            .min_by_key(|(_, set)| set.last_used)
            .map(|(id, _)| Arc::clone(id))
    {
        sets.remove(&oldest);
    }
    sets.insert(
        set_id,
        LoadedSet {
            lookup: Arc::new(lookup),
            last_used: Instant::now(),
        },
    );
}

fn prune_users() {
    let mut by_recency: Vec<(u64, Arc<str>)> = USER_SETS
        .iter()
        .map(|entry| {
            (
                entry.last_seen_ms.load(Ordering::Relaxed),
                Arc::clone(entry.key()),
            )
        })
        .collect();
    let excess = by_recency.len().saturating_sub(USERS_MAX * 9 / 10);
    if excess == 0 {
        return;
    }
    by_recency.select_nth_unstable_by_key(excess - 1, |(last_seen, _)| *last_seen);
    for (_, user_id) in by_recency.into_iter().take(excess) {
        USER_SETS.remove(&user_id);
    }
}

/// Drop personal emotes of users no longer tracked by the cosmetics cache.
pub fn forget_user(user_id: &str) {
    USER_SETS.remove(user_id);
}

/// Drop expired failure and fetch records so a long session does not
/// accumulate them.
pub fn prune() {
    FAILED_SETS.retain(|_, failed_at| failed_at.elapsed() < FAILED_SET_RETRY);
    LAST_FETCH.retain(|_, fetched_at| fetched_at.elapsed() < SET_REFETCH_INTERVAL);
}
