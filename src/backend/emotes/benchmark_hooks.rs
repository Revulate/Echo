//! Hooks for the UI fixture and the provider-index bench. The fixture hooks
//! are no-ops unless `ECHO_BENCH_UI_FIXTURE` is on; the provider-index bench
//! only exists with the `provider-index-bench` feature.

use super::*;

/// Install a tiny process-local emote set for the opt-in native completion
/// interaction harness. Synthetic fixture channels never persist this cache.
pub(crate) fn seed_completion_emotes_for_benchmark(channel_login: &str) -> bool {
    if !crate::bench::ui_fixture::is_fixture_channel(channel_login) {
        return false;
    }

    let cache_key = normalize_channel_key(channel_login);
    let channel: Arc<str> = Arc::from(channel_login);
    let mut emotes = HashMap::new();
    let mut emotes_by_id = EmoteIdentityMap::default();
    for (index, name) in crate::bench::ui_fixture::COMPLETION_EMOTES
        .iter()
        .enumerate()
    {
        let id: Arc<str> = Arc::from(format!("echo-fixture-completion-{index}"));
        let url: Arc<str> = Arc::from(format!(
            "https://static-cdn.jtvnw.net/emoticons/v2/{}/default/dark/2.0",
            25 + index
        ));
        EmoteCache::insert_emote_into_maps(
            &mut emotes,
            &mut emotes_by_id,
            Emote {
                id,
                name: Arc::from(*name),
                url: Arc::clone(&url),
                provider: EmoteProvider::Twitch,
                channel: Some(Arc::clone(&channel)),
                original_name: None,
                creator_name: None,
                twitch_owner_login: Some(Arc::clone(&channel)),
                twitch_owner_id: None,
                twitch_emote_type: None,
                url_1x: Some(Arc::clone(&url)),
                url_2x: Some(url),
                url_4x: None,
                zero_width: false,
                is_animated: false,
                width: Some(28),
                height: Some(28),
            },
        );
    }

    let lookup = Arc::new(emotes.clone());
    let publication = channel_emote_publication(&cache_key);
    let publication_epoch = publication.begin_rebuild();
    EMOTE_CACHE.channels.insert(
        cache_key.clone(),
        CacheEntry {
            emotes,
            emotes_by_id,
            fetched_at: Instant::now(),
        },
    );
    let _commit = publication.commit_lock.lock_or_recover();
    if publication.epoch.load(Ordering::Acquire) != publication_epoch {
        return false;
    }
    publication.lookup.store(lookup);
    increment_channel_version(channel_login);
    true
}

pub(crate) fn clear_completion_emotes_for_benchmark(channel_login: &str) -> bool {
    if !crate::bench::ui_fixture::is_enabled() {
        return false;
    }
    let cache_key = normalize_channel_key(channel_login);
    let publication = channel_emote_publication(&cache_key);
    publication.invalidate();
    let _commit = publication.commit_lock.lock_or_recover();
    let removed = EMOTE_CACHE.channels.remove(&cache_key).is_some();
    publication.lookup.store(Arc::new(HashMap::new()));
    if removed {
        increment_channel_version(channel_login);
    }
    removed
}

/// Production-code timing result for the provider-index evaluator.
#[cfg(feature = "provider-index-bench")]
pub struct ProviderIndexBenchResult {
    pub entries: usize,
    pub iterations: usize,
    pub equivalent: bool,
    pub full_median_ns: u64,
    pub incremental_median_ns: u64,
}

#[cfg(feature = "provider-index-bench")]
impl ProviderIndexBenchResult {
    pub fn speedup(&self) -> f64 {
        self.full_median_ns as f64 / self.incremental_median_ns as f64
    }
}

/// Measure the actual production full and incremental provider-index paths.
#[cfg(feature = "provider-index-bench")]
pub fn benchmark_provider_index_refresh(iterations: usize) -> ProviderIndexBenchResult {
    fn fixture_emote(provider: EmoteProvider, index: usize, name: Arc<str>) -> Arc<Emote> {
        let id: Arc<str> = Arc::from(format!("{provider:?}-{index}"));
        Arc::new(Emote {
            url: Arc::from(format!("https://example.invalid/{id}")),
            id,
            name,
            provider,
            channel: None,
            original_name: None,
            creator_name: None,
            twitch_owner_login: None,
            twitch_owner_id: None,
            twitch_emote_type: None,
            url_1x: None,
            url_2x: None,
            url_4x: None,
            zero_width: false,
            is_animated: false,
            width: Some(28),
            height: Some(28),
        })
    }

    fn median(mut values: Vec<u64>) -> u64 {
        values.sort_unstable();
        values[values.len() / 2]
    }

    let providers = [
        EmoteProvider::Twitch,
        EmoteProvider::FFZ,
        EmoteProvider::BTTV,
        EmoteProvider::SevenTV,
    ];
    let initial = providers.into_iter().flat_map(|provider| {
        (0..1025).map(move |index| {
            let name = if index == 12 {
                Arc::from("Collision")
            } else {
                Arc::from(format!("{provider:?}_{index}"))
            };
            fixture_emote(provider, index, name)
        })
    });
    let emotes_by_id = initial.collect::<EmoteIdentityMap>();
    let mut initial_entry = CacheEntry {
        emotes: HashMap::new(),
        emotes_by_id,
        fetched_at: Instant::now(),
    };
    EmoteCache::rebuild_name_map_from_identities(&mut initial_entry);
    let replacement = (0..1025)
        .filter(|index| *index != 40)
        .map(|index| {
            let name: Arc<str> = match index {
                12 => Arc::from("RenamedCollision"),
                41 => Arc::from("HUH"),
                _ => Arc::from(format!("SevenTV_{index}")),
            };
            fixture_emote(EmoteProvider::SevenTV, index, name)
        })
        .chain([fixture_emote(
            EmoteProvider::SevenTV,
            1025,
            Arc::from("huh"),
        )])
        .collect::<Vec<_>>();

    let iterations = iterations.max(1);
    let mut full_samples = Vec::with_capacity(iterations);
    let mut incremental_samples = Vec::with_capacity(iterations);
    let mut equivalent = true;
    for _ in 0..iterations {
        let mut full = initial_entry.clone();
        let full_started = Instant::now();
        let previous_provider = full
            .emotes_by_id
            .0
            .remove(&EmoteProvider::SevenTV)
            .unwrap_or_default();
        for emote in &replacement {
            let emote = find_emote_alias(&previous_provider, &emote.id, &emote.name)
                .filter(|previous| previous.as_ref() == emote.as_ref())
                .cloned()
                .unwrap_or_else(|| Arc::clone(emote));
            full.emotes_by_id.insert(emote);
        }
        EmoteCache::rebuild_name_map_from_identities(&mut full);
        full_samples.push(full_started.elapsed().as_nanos().min(u128::from(u64::MAX)) as u64);

        let mut incremental = initial_entry.clone();
        let incremental_started = Instant::now();
        let incremental_diff = EmoteCache::replace_provider_rows_incrementally(
            &mut incremental,
            EmoteProvider::SevenTV,
            replacement.iter().cloned(),
        )
        .expect("benchmark fixture must remain inside the incremental threshold");
        incremental_samples.push(
            incremental_started
                .elapsed()
                .as_nanos()
                .min(u128::from(u64::MAX)) as u64,
        );

        equivalent &= incremental.emotes == full.emotes
            && incremental_diff.provider == EmoteProvider::SevenTV;
    }

    ProviderIndexBenchResult {
        entries: initial_entry.emotes_by_id.len(),
        iterations,
        equivalent,
        full_median_ns: median(full_samples),
        incremental_median_ns: median(incremental_samples),
    }
}
