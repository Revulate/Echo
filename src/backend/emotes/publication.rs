//! Per-channel emote catalog publication: the lookup maps chat ingestion
//! reads, their rebuilds from the emote cache, and channel open/close
//! bookkeeping.

use super::*;

/// Emotes indexed by channel for fast iteration (channel_login -> Vec<Arc<Emote>>)
pub static EMOTE_BY_CHANNEL: LazyLock<RwLock<HashMap<String, Vec<Arc<Emote>>>>> =
    LazyLock::new(|| RwLock::new(HashMap::new()));
pub(super) static CHANNEL_EMOTE_CATALOG_VERSIONS: LazyLock<DashMap<String, u32>> =
    LazyLock::new(DashMap::new);
pub(super) static PROVIDER_VISIBILITY_GENERATION: AtomicU64 = AtomicU64::new(0);
pub(super) static PROVIDER_VISIBILITY_APPLY_LOCK: LazyLock<tokio::sync::Mutex<()>> =
    LazyLock::new(|| tokio::sync::Mutex::new(()));

/// Stable per-channel derived-index publication slot.
///
/// The registry below is only copied when a channel receives its first slot.
/// Every subsequent replacement (including an empty removal tombstone) swaps
/// this slot's shared inner lookup in place. Readers never touch the writer
/// epoch or commit lock.
pub struct ChannelEmotePublication {
    pub(super) epoch: AtomicU64,
    pub(super) tombstoned: AtomicBool,
    pub(super) commit_lock: Mutex<()>,
    pub(super) lookup: ArcSwap<EmoteNameMap>,
}

impl ChannelEmotePublication {
    pub(super) fn new() -> Self {
        Self {
            epoch: AtomicU64::new(0),
            tombstoned: AtomicBool::new(false),
            commit_lock: Mutex::new(()),
            lookup: ArcSwap::from_pointee(HashMap::new()),
        }
    }

    #[inline]
    pub(super) fn begin_rebuild(&self) -> u64 {
        self.epoch.fetch_add(1, Ordering::AcqRel).wrapping_add(1)
    }

    #[inline]
    pub(super) fn invalidate(&self) {
        self.epoch.fetch_add(1, Ordering::AcqRel);
    }

    #[inline]
    pub(crate) fn lookup_snapshot(&self) -> Arc<EmoteNameMap> {
        self.lookup.load_full()
    }
}

/// Mark a channel publication active at the explicit connection boundary.
pub fn activate_channel_emotes(channel: &str) {
    let cache_key = normalize_channel_key(channel);
    let publication = channel_emote_publication(&cache_key);
    let _commit = publication.commit_lock.lock_or_recover();
    publication.invalidate();
    publication.tombstoned.store(false, Ordering::Release);
}

/// Lock-free channel lookup registry. Entries intentionally remain after a
/// removal as empty tombstone slots so an already-running rebuild cannot
/// recreate a closed channel's derived indices. The outer map is cloned only
/// when a channel is registered for the first time; all ordinary publications
/// update the slot's inner [`ArcSwap`] instead.
pub static EMOTE_LOOKUP_MAPS: LazyLock<ArcSwap<HashMap<String, Arc<ChannelEmotePublication>>>> =
    LazyLock::new(|| ArcSwap::from_pointee(HashMap::new()));

#[inline]
pub(super) fn channel_emote_publication(cache_key: &str) -> Arc<ChannelEmotePublication> {
    if let Some(publication) = EMOTE_LOOKUP_MAPS.load().get(cache_key) {
        return Arc::clone(publication);
    }

    let candidate = Arc::new(ChannelEmotePublication::new());
    EMOTE_LOOKUP_MAPS.rcu(|publications| {
        if publications.contains_key(cache_key) {
            return Arc::clone(publications);
        }
        let mut updated = (**publications).clone();
        updated.insert(cache_key.to_string(), Arc::clone(&candidate));
        Arc::new(updated)
    });

    EMOTE_LOOKUP_MAPS
        .load()
        .get(cache_key)
        .cloned()
        .expect("channel publication slot registered")
}

#[inline]
pub(super) fn channel_emote_publication_if_registered(
    cache_key: &str,
) -> Option<Arc<ChannelEmotePublication>> {
    EMOTE_LOOKUP_MAPS.load().get(cache_key).cloned()
}

/// Drop a closed channel's renderer vector and replace its lookup with an empty
/// tombstone. Safe to call even if the channel is reopened later — the stable
/// slot receives a newer epoch and publishes normally on the next provider load.
pub fn remove_channel_emotes(channel: &str) {
    let cache_key = normalize_channel_key(channel);
    FAILED_CHANNELS.remove(&cache_key);
    if CHANNEL_CATALOG_PENDING.remove(&cache_key).is_some() {
        EMOTE_CACHE.channels.remove(&cache_key);
        EMOTE_CACHE.channel_emote_set_ids.remove(&cache_key);
    } else {
        retain_closed_channel_catalog(&cache_key);
    }
    let publication = channel_emote_publication(&cache_key);
    publication.invalidate();
    let _commit = publication.commit_lock.lock_or_recover();
    publication.tombstoned.store(true, Ordering::Release);
    EMOTE_BY_CHANNEL.write_or_recover().remove(&cache_key);

    publication.lookup.store(Arc::new(HashMap::new()));
}

const MAX_CLOSED_CHANNEL_CATALOGS: usize = 32;

static CLOSED_CHANNEL_CATALOGS: LazyLock<Mutex<std::collections::VecDeque<String>>> =
    LazyLock::new(|| Mutex::new(std::collections::VecDeque::new()));

fn retain_closed_channel_catalog(cache_key: &str) {
    let evicted: Vec<String> = {
        let mut closed = CLOSED_CHANNEL_CATALOGS.lock_or_recover();
        closed.retain(|key| key != cache_key && crate::backend::irc::is_channel_closed(key));
        closed.push_back(cache_key.to_string());
        let excess = closed.len().saturating_sub(MAX_CLOSED_CHANNEL_CATALOGS);
        closed.drain(..excess).collect()
    };
    for key in evicted {
        if crate::backend::irc::is_channel_closed(&key) {
            EMOTE_CACHE.channels.remove(&key);
            EMOTE_CACHE.channel_emote_set_ids.remove(&key);
        }
    }
}

pub(super) fn trigger_open_channel_preload(cache_key: &str) {
    if !crate::backend::irc::is_channel_closed(cache_key) {
        crate::cache::loader::trigger_channel_preload(cache_key);
    }
}

/// True when the channel has a non-empty fast-lookup entry, i.e. tokenization can
/// currently resolve its emotes. Used by the recovery paths (staggered-load retry,
/// open watchdog) to decide whether a lookup gap still needs healing.
pub fn channel_has_lookup_entry(channel: &str) -> bool {
    let cache_key = normalize_channel_key(channel);
    channel_emote_publication_if_registered(&cache_key)
        .is_some_and(|publication| !publication.lookup_snapshot().is_empty())
}

/// Whether a channel has a catalog safe to use for one-time history parsing.
/// A non-empty partial FFZ/BTTV lookup is insufficient while a cold channel's
/// deferred 7TV fetch remains pending; an authoritative empty cache entry is
/// safe and prevents waiting forever for a channel with no provider emotes.
pub fn channel_catalog_ready_for_history(channel: &str) -> bool {
    let cache_key = normalize_channel_key(channel);
    let Some(entry) = EMOTE_CACHE.channels.get(&cache_key) else {
        return false;
    };
    if CHANNEL_CATALOG_PENDING
        .get(&cache_key)
        .is_some_and(|token| token.load(Ordering::Acquire))
    {
        return false;
    }
    entry.emotes_by_id.len() == 0 || channel_has_lookup_entry(&cache_key)
}

/// True while a cold channel's initial provider publication is incomplete.
/// History uses this to await the existing owner instead of restarting a
/// partial catalog load after its short retry backdate.
pub fn channel_initial_catalog_pending(channel: &str) -> bool {
    let cache_key = normalize_channel_key(channel);
    CHANNEL_CATALOG_PENDING
        .get(&cache_key)
        .is_some_and(|token| token.load(Ordering::Acquire))
}

/// One-shot open watchdog: roughly [`CHANNEL_OPEN_EMOTE_WATCHDOG_SECS`] after a
/// channel open/load is kicked off, if the channel still has no lookup entry,
/// self-heal. Rebuilds the index from cached metadata when it is resident (the
/// common reopen case, where the entry was pruned on close) and only re-triggers
/// a full network load when the cache is empty too. Runs on the durable runtime.
pub fn spawn_channel_open_emote_watchdog(channel: &str) {
    let channel_key = normalize_channel_key(channel);
    if channel_key.is_empty()
        || channel_key == "whisper"
        || crate::bench::ui_fixture::is_fixture_channel(&channel_key)
    {
        return;
    }
    crate::backend::bridge::get_runtime().spawn(async move {
        tokio::time::sleep(Duration::from_secs(CHANNEL_OPEN_EMOTE_WATCHDOG_SECS)).await;
        if channel_has_lookup_entry(&channel_key) {
            return;
        }
        crate::log_emotes!(
            warn,
            "[HEAL] {} has no emote lookup map {}s after open; self-healing",
            channel_key,
            CHANNEL_OPEN_EMOTE_WATCHDOG_SECS
        );
        if EMOTE_CACHE.channel_emote_count(&channel_key) > 0 {
            rebuild_emote_indices(&channel_key).await;
        } else {
            crate::backend::bridge::preload_channel_emotes_predictive(&channel_key);
        }
    });
}

/// Update the dual-index caches from EmoteCache
pub async fn rebuild_emote_indices(channel_login: &str) {
    rebuild_emote_indices_inner(channel_login).await;
}

/// Publish a channel snapshot while holding the channel writer lock.
///
/// This is the uncommon race fallback: building the authoritative snapshot
/// inside the serialized writer section guarantees that a stale incremental or
/// full rebuild cannot leave a cache mutation absent from the published index.
pub(super) fn publish_authoritative_channel_snapshot(cache_key: &str) -> usize {
    publish_authoritative_channel_snapshot_with_hook(cache_key, |_| {})
}

pub(super) fn publish_authoritative_channel_snapshot_with_hook(
    cache_key: &str,
    after_begin: impl FnOnce(&Arc<ChannelEmotePublication>),
) -> usize {
    let publication = channel_emote_publication(cache_key);
    let _commit = publication.commit_lock.lock_or_recover();
    publication.begin_rebuild();
    after_begin(&publication);
    if publication.tombstoned.load(Ordering::Acquire) {
        EMOTE_BY_CHANNEL.write_or_recover().remove(cache_key);
        publication.lookup.store(Arc::new(HashMap::new()));
        clear_channel_pending(cache_key);
        publication.invalidate();
        return 0;
    }
    let (emotes, lookup) = EMOTE_CACHE.get_channel_index_snapshot(cache_key);
    let emote_name_count = lookup.len();
    EMOTE_BY_CHANNEL
        .write_or_recover()
        .insert(cache_key.to_string(), emotes);
    publication.lookup.store(Arc::new(lookup));
    clear_channel_pending(cache_key);
    publication.invalidate();
    emote_name_count
}

pub(super) async fn publish_channel_provider_diff(channel_login: &str, diff: ProviderIndexDiff) {
    if [
        EmoteProvider::Twitch,
        EmoteProvider::SevenTV,
        EmoteProvider::BTTV,
        EmoteProvider::FFZ,
        EmoteProvider::Emoji,
    ]
    .into_iter()
    .any(|provider| !emote_provider_enabled(provider))
    {
        rebuild_emote_indices(channel_login).await;
        return;
    }

    let cache_key = normalize_channel_key(channel_login);
    let publication = channel_emote_publication(&cache_key);
    let publication_epoch = publication.begin_rebuild();
    let mut lookup = (*publication.lookup_snapshot()).clone();
    let provider_emotes = if let Some(entry) = EMOTE_CACHE.channels.get(&cache_key) {
        for alias in &diff.changed_aliases {
            if let Some(emote) = entry.emotes.get(alias.as_ref()) {
                lookup.insert(Arc::clone(alias), Arc::clone(emote));
            } else {
                lookup.remove(alias.as_ref());
            }
        }
        entry
            .emotes_by_id
            .0
            .get(&diff.provider)
            .into_iter()
            .flat_map(|map| map.values().flatten())
            .cloned()
            .collect::<Vec<_>>()
    } else {
        rebuild_emote_indices(channel_login).await;
        return;
    };

    let published = {
        let _commit = publication.commit_lock.lock_or_recover();
        if publication.epoch.load(Ordering::Acquire) != publication_epoch
            || publication.tombstoned.load(Ordering::Acquire)
        {
            false
        } else {
            let mut by_channel = EMOTE_BY_CHANNEL.write_or_recover();
            let emotes = by_channel.entry(cache_key.clone()).or_default();
            emotes.retain(|emote| emote.provider != diff.provider);
            emotes.extend(provider_emotes);
            publication.lookup.store(Arc::new(lookup));
            clear_channel_pending(&cache_key);
            true
        }
    };

    if !published {
        let emote_name_count = publish_authoritative_channel_snapshot(&cache_key);
        crate::log_emotes!(
            debug,
            "Rebuilt stale incremental channel emote publication for {} (epoch {}, names={})",
            cache_key,
            publication_epoch,
            emote_name_count
        );
    }

    increment_channel_catalog_version(&cache_key);
}

async fn rebuild_emote_indices_inner(channel_login: &str) {
    let cache_key = normalize_channel_key(channel_login);
    let publication = channel_emote_publication(&cache_key);
    let publication_epoch = publication.begin_rebuild();

    let (emotes, lookup) = EMOTE_CACHE.get_channel_index_snapshot(channel_login);
    let emote_name_count = lookup.len();

    let audit_channel = echo_core::env::value("ECHO_AUDIT_CHANNEL");
    let audit_tokens = echo_core::env::value("ECHO_AUDIT_EMOTES");
    if audit_channel
        .as_deref()
        .map(normalize_channel_key)
        .as_deref()
        == Some(cache_key.as_str())
        && let Some(tokens_csv) = audit_tokens
    {
        for token in tokens_csv
            .split(',')
            .map(str::trim)
            .filter(|t| !t.is_empty())
        {
            match lookup.get(token) {
                Some(emote) => crate::log_emotes!(
                    info,
                    "[AUDIT] channel={} token='{}' -> HIT provider={:?} id={} url={}",
                    cache_key,
                    token,
                    emote.provider,
                    emote.id,
                    emote.url
                ),
                None => crate::log_emotes!(
                    info,
                    "[AUDIT] channel={} token='{}' -> MISS",
                    cache_key,
                    token
                ),
            }
        }
    }

    let published = {
        let _commit = publication.commit_lock.lock_or_recover();
        if publication.epoch.load(Ordering::Acquire) != publication_epoch
            || publication.tombstoned.load(Ordering::Acquire)
        {
            crate::log_emotes!(
                debug,
                "Discarding stale channel emote index publication for {} (epoch {})",
                cache_key,
                publication_epoch
            );
            false
        } else {
            EMOTE_BY_CHANNEL
                .write_or_recover()
                .insert(cache_key.clone(), emotes);

            publication.lookup.store(Arc::new(lookup));

            clear_channel_pending(&cache_key);
            true
        }
    };

    if !published {
        let recovered_name_count = publish_authoritative_channel_snapshot(&cache_key);
        crate::log_emotes!(
            debug,
            "Recovered stale channel emote index publication for {} (epoch {}, names={})",
            cache_key,
            publication_epoch,
            recovered_name_count
        );
    }

    crate::log_emotes!(
        info,
        "rebuild_emote_indices for {}: {} emotes loaded, preserving resident rows",
        cache_key,
        emote_name_count
    );
    increment_channel_catalog_version(&cache_key);
}

fn emote_lookup_changed(previous: &EmoteNameMap, replacement: &EmoteNameMap) -> bool {
    previous.len() != replacement.len()
        || previous.iter().any(|(name, previous_emote)| {
            replacement
                .get(name.as_ref())
                .is_none_or(|replacement_emote| !Arc::ptr_eq(previous_emote, replacement_emote))
        })
}

/// Update global emote indices
pub async fn rebuild_global_emote_index() {
    let emotes = EMOTE_CACHE.get_global_emotes();

    let mut lookup: EmoteNameMap = HashMap::with_capacity(emotes.len() * 2);
    for emote in &emotes {
        EmoteCache::insert_emote_name_keys(&mut lookup, Arc::clone(emote));
    }

    let emote_count = lookup.len();

    crate::log_emotes!(
        info,
        "Global emote index rebuilt: {} emotes registered",
        emote_count
    );

    let previous_lookup = GLOBAL_EMOTES_LOOKUP.load_full();
    let replacement_lookup = Arc::new(lookup);
    let effective_lookup_changed = emote_lookup_changed(&previous_lookup, &replacement_lookup);
    *GLOBAL_EMOTES_LIST.write_or_recover() = emotes;
    GLOBAL_EMOTES_LOOKUP.store(replacement_lookup);

    if effective_lookup_changed {
        crate::state::VersionRegistry::bump_global_emote_version();
        publish_emote_metadata_change();
    }
}

/// Update user emote indices (personal 7TV, subscriptions)
pub async fn rebuild_user_emote_index() {
    let emotes = EMOTE_CACHE.get_user_emotes();

    let mut lookup: EmoteNameMap = HashMap::with_capacity(emotes.len() * 2);
    for emote in &emotes {
        EmoteCache::insert_emote_name_keys(&mut lookup, Arc::clone(emote));
    }

    let previous_lookup = USER_EMOTES_LOOKUP.load_full();
    let replacement_lookup = Arc::new(lookup);
    let effective_lookup_changed = emote_lookup_changed(&previous_lookup, &replacement_lookup);
    USER_EMOTES_LOOKUP.store(replacement_lookup);

    if effective_lookup_changed {
        crate::state::VersionRegistry::bump_user_emote_version();
        publish_emote_metadata_change();
    }
}
