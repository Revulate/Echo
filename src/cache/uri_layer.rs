//! App-scoped, bounded cache for GPUI URI images.
//!
//! The stock GPUI URI cache is retain-all. Echo's dedicated emote caches already
//! bound their media; this layer gives every remaining `img(uri)` a shared LRU,
//! byte/count cap, and explicit atlas retirement.

use futures_util::future::FutureExt as _;
use gpui::{
    App, AppContext as _, Asset as _, AssetLogger, Entity, EntityId, Global, ImageAssetLoader,
    ImageCache, ImageCacheError, ImageCacheProvider, RenderImage, Resource, Window, WindowId,
};
use parking_lot::RwLock;
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
    sync::LazyLock,
    time::{Duration, Instant},
};

const MAX_BYTES: usize = 64 * 1024 * 1024;
const MAX_ENTRIES: usize = 512;
const MAX_INFLIGHT: usize = 2;
const MAX_FAILURES: usize = 64;
const MAX_WAITERS_PER_RESOURCE: usize = 32;
const MAX_SLOT_WAITERS: usize = 256;
const MAX_TRACKED_WINDOWS: usize = 32;
const MAX_VISIBLE_REFERENCES: usize = MAX_ENTRIES * 4;
const FAILURE_TTL: Duration = Duration::from_secs(30);

/// Thread-safe diagnostic snapshot. The cache itself remains owned by GPUI's
/// app thread; this only lets periodic process-memory reporting observe it.
static DIAGNOSTIC_STATS: LazyLock<RwLock<UriImageCacheStats>> =
    LazyLock::new(|| RwLock::new(UriImageCacheStats::default()));

enum Entry {
    Loading {
        token: u64,
        waiters: HashSet<EntityId>,
    },
    Loaded {
        result: Result<Arc<RenderImage>, ImageCacheError>,
        bytes: usize,
        last_used: u64,
        completed: Instant,
        charged: usize,
    },
}

#[derive(Clone, Copy, Debug, Default)]
pub struct UriImageCacheStats {
    pub resident_bytes: usize,
    pub resident_entries: usize,
    pub inflight: usize,
    pub hits: u64,
    pub misses: u64,
    pub evictions: u64,
    pub retirements: u64,
    pub failures: u64,
    pub rejections: u64,
    pub failed_entries: usize,
    pub pinned_entries: usize,
    pub pinned_bytes: usize,
    pub waiter_rejections: u64,
    pub late_completion_discards: u64,
    pub pending_retirements: usize,
    pub tracked_windows: usize,
    pub visible_references: usize,
    pub window_cap_rejections: u64,
    pub visible_cap_rejections: u64,
}

#[derive(Default)]
struct Generations {
    current: HashSet<Resource>,
    previous: HashSet<Resource>,
}

/// Mutated exclusively from GPUI's app/render thread.
pub struct EchoUriImageCache {
    entries: HashMap<Resource, Entry>,
    visible: HashMap<WindowId, Generations>,
    slot_waiters: HashSet<EntityId>,
    next_token: u64,
    clock: u64,
    max_bytes: usize,
    budget: Arc<super::budget::BudgetManager>,
    retry_scheduled: HashSet<EntityId>,
    stats: UriImageCacheStats,
}

impl EchoUriImageCache {
    fn new() -> Self {
        let requested = echo_core::env::parsed::<usize>("ECHO_GPUI_URI_CACHE_MB")
            .map(|mb| mb.saturating_mul(1024 * 1024))
            .unwrap_or_else(|| super::budget::default_total_budget_bytes() / 4);
        let shared_ceiling =
            (super::budget::default_total_budget_bytes() / 4).clamp(16 * 1024 * 1024, MAX_BYTES);
        Self {
            entries: HashMap::new(),
            visible: HashMap::new(),
            slot_waiters: HashSet::new(),
            next_token: 1,
            clock: 1,
            max_bytes: requested.clamp(16 * 1024 * 1024, shared_ceiling),
            budget: super::loader::shared_decoded_media_budget(),
            retry_scheduled: HashSet::new(),
            stats: UriImageCacheStats::default(),
        }
    }

    fn begin_generation(&mut self, id: WindowId) {
        if !self.visible.contains_key(&id) && self.visible.len() >= MAX_TRACKED_WINDOWS {
            self.refresh_visibility_stats();
            self.publish_stats();
            return;
        }
        let generations = self.visible.entry(id).or_default();
        generations.previous = std::mem::take(&mut generations.current);
        self.refresh_visibility_stats();
        self.publish_stats();
    }
    fn visible_references(&self) -> usize {
        self.visible
            .values()
            .map(|generations| generations.current.len() + generations.previous.len())
            .sum()
    }
    /// Returns false rather than growing unbounded per-window or process-wide
    /// pin sets. An untracked root must keep its URI placeholders: returning a
    /// cached image without a pin would make it eligible for premature atlas
    /// retirement while it is still painted.
    fn mark_visible(&mut self, id: WindowId, resource: &Resource) -> bool {
        if !self.visible.contains_key(&id) && self.visible.len() >= MAX_TRACKED_WINDOWS {
            self.stats.window_cap_rejections = self.stats.window_cap_rejections.saturating_add(1);
            return false;
        }
        let visible_references = self.visible_references();
        let generations = self.visible.entry(id).or_default();
        if generations.current.contains(resource) {
            return true;
        }
        if generations.current.len() >= MAX_ENTRIES || visible_references >= MAX_VISIBLE_REFERENCES
        {
            self.stats.visible_cap_rejections = self.stats.visible_cap_rejections.saturating_add(1);
            return false;
        }
        generations.current.insert(resource.clone());
        self.refresh_visibility_stats();
        true
    }
    fn remove_window(&mut self, id: WindowId) -> HashSet<EntityId> {
        self.visible.remove(&id);
        self.refresh();
        let waiters = std::mem::take(&mut self.slot_waiters);
        for waiter in &waiters {
            self.retry_scheduled.remove(waiter);
        }
        waiters
    }
    fn pinned(&self, resource: &Resource) -> bool {
        self.visible
            .values()
            .any(|g| g.current.contains(resource) || g.previous.contains(resource))
    }
    fn tick(&mut self) -> u64 {
        self.clock = self.clock.wrapping_add(1);
        self.clock
    }
    fn bytes(image: &RenderImage) -> usize {
        (0..image.frame_count()).map(|n| image.byte_len(n)).sum()
    }
    fn notify(cx: &mut App, waiters: impl IntoIterator<Item = EntityId>) {
        for waiter in waiters {
            cx.notify(waiter);
        }
    }
    /// Re-render `waiters` once a shared-budget rejection has expired, so a view
    /// that only redraws when notified retries after memory frees up. Images
    /// larger than the whole cache never fit and are not woken.
    fn notify_after_failure_ttl(cx: &mut App, waiters: HashSet<EntityId>) {
        cx.spawn(async move |cx| {
            cx.background_executor()
                .timer(FAILURE_TTL + Duration::from_millis(100))
                .await;
            cx.update(|cx| Self::notify(cx, waiters));
        })
        .detach();
    }
    fn refresh(&mut self) {
        self.stats.resident_entries = self
            .entries
            .values()
            .filter(|entry| matches!(entry, Entry::Loaded { result: Ok(_), .. }))
            .count();
        self.stats.resident_bytes = self
            .entries
            .values()
            .filter_map(|entry| match entry {
                Entry::Loaded {
                    result: Ok(_),
                    bytes,
                    ..
                } => Some(*bytes),
                _ => None,
            })
            .sum();
        self.stats.inflight = self
            .entries
            .values()
            .filter(|entry| matches!(entry, Entry::Loading { .. }))
            .count();
        self.stats.failed_entries = self
            .entries
            .values()
            .filter(|entry| matches!(entry, Entry::Loaded { result: Err(_), .. }))
            .count();
        let (pinned_entries, pinned_bytes) =
            self.entries
                .iter()
                .fold(
                    (0usize, 0usize),
                    |(count, bytes), (resource, entry)| match entry {
                        Entry::Loaded {
                            result: Ok(_),
                            bytes: entry_bytes,
                            ..
                        } if self.pinned(resource) => (count + 1, bytes + entry_bytes),
                        _ => (count, bytes),
                    },
                );
        self.stats.pinned_entries = pinned_entries;
        self.stats.pinned_bytes = pinned_bytes;
        self.refresh_visibility_stats();
        self.stats.pending_retirements = 0;
        self.publish_stats();
    }
    fn refresh_visibility_stats(&mut self) {
        self.stats.tracked_windows = self.visible.len();
        self.stats.visible_references = self.visible_references();
    }
    fn publish_stats(&self) {
        *DIAGNOSTIC_STATS.write() = self.stats;
    }
    fn schedule_one_shot_retry(&mut self, waiter: EntityId, window: &mut Window, _cx: &mut App) {
        if self.retry_scheduled.len() >= MAX_SLOT_WAITERS && !self.retry_scheduled.contains(&waiter)
        {
            self.stats.waiter_rejections = self.stats.waiter_rejections.saturating_add(1);
            self.publish_stats();
            return;
        }
        if !self.retry_scheduled.insert(waiter) {
            return;
        }
        window.on_next_frame(move |_window, cx| {
            cx.notify(waiter);
        });
    }
    fn queue_slot_waiter(&mut self, waiter: EntityId) {
        if self.slot_waiters.len() < MAX_SLOT_WAITERS || self.slot_waiters.contains(&waiter) {
            self.slot_waiters.insert(waiter);
        } else {
            self.stats.waiter_rejections = self.stats.waiter_rejections.saturating_add(1);
        }
    }
    fn notify_slot_waiters(&mut self, cx: &mut App) {
        let waiters = std::mem::take(&mut self.slot_waiters);
        for waiter in waiters {
            self.retry_scheduled.remove(&waiter);
            cx.notify(waiter);
        }
    }
    fn eviction_candidate(&self) -> Option<Resource> {
        self.entries
            .iter()
            .filter_map(|(resource, entry)| match entry {
                Entry::Loaded {
                    result: Ok(_),
                    last_used,
                    ..
                } if !self.pinned(resource) => Some((resource.clone(), *last_used)),
                Entry::Loaded { result: Err(_), .. } if !self.pinned(resource) => {
                    Some((resource.clone(), 0))
                }
                _ => None,
            })
            .min_by_key(|(_, age)| *age)
            .map(|(resource, _)| resource)
    }
    fn remove_entry(&mut self, resource: &Resource) {
        if let Some(Entry::Loaded {
            result: Ok(image),
            charged,
            ..
        }) = self.entries.remove(resource)
        {
            self.budget
                .release(super::budget::CacheLayer::Static, charged);
            super::queue_render_image_drop(image);
            self.stats.retirements = self.stats.retirements.saturating_add(1);
        }
    }
    fn evict_until_fit(&mut self, extra_bytes: usize, extra_entries: usize) -> bool {
        self.refresh();
        while self.entries.len().saturating_add(extra_entries) > MAX_ENTRIES
            || self.stats.resident_bytes.saturating_add(extra_bytes) > self.max_bytes
        {
            let victim = self.eviction_candidate();
            let Some(victim) = victim else {
                return false;
            };
            self.remove_entry(&victim);
            self.stats.evictions = self.stats.evictions.saturating_add(1);
            self.refresh();
        }
        true
    }
    fn allocate_shared_bytes(&mut self, bytes: usize) -> bool {
        while !self
            .budget
            .try_allocate(super::budget::CacheLayer::Static, bytes)
        {
            let Some(victim) = self.eviction_candidate() else {
                return false;
            };
            self.remove_entry(&victim);
            self.stats.evictions = self.stats.evictions.saturating_add(1);
        }
        true
    }
    fn trim_failures(&mut self) {
        while self
            .entries
            .values()
            .filter(|entry| matches!(entry, Entry::Loaded { result: Err(_), .. }))
            .count()
            > MAX_FAILURES
        {
            let victim = self.entries.iter().find_map(|(resource, entry)| {
                (matches!(entry, Entry::Loaded { result: Err(_), .. }) && !self.pinned(resource))
                    .then_some(resource.clone())
            });
            let Some(victim) = victim else {
                break;
            };
            self.entries.remove(&victim);
        }
    }
    fn reserve_failure_slot(&mut self) -> bool {
        while self
            .entries
            .values()
            .filter(|entry| matches!(entry, Entry::Loaded { result: Err(_), .. }))
            .count()
            >= MAX_FAILURES
        {
            let victim = self.entries.iter().find_map(|(resource, entry)| {
                (matches!(entry, Entry::Loaded { result: Err(_), .. }) && !self.pinned(resource))
                    .then_some(resource.clone())
            });
            let Some(victim) = victim else {
                return false;
            };
            self.entries.remove(&victim);
        }
        true
    }
    fn complete(
        &mut self,
        resource: Resource,
        token: u64,
        result: Result<Arc<RenderImage>, ImageCacheError>,
        cx: &mut App,
    ) {
        let waiters = match self.entries.get(&resource) {
            Some(Entry::Loading {
                token: current,
                waiters,
                ..
            }) if *current == token => waiters.clone(),
            _ => {
                self.stats.late_completion_discards =
                    self.stats.late_completion_discards.saturating_add(1);
                if let Ok(image) = result {
                    cx.drop_image(image, None);
                    self.stats.retirements = self.stats.retirements.saturating_add(1);
                }
                self.refresh();
                return;
            }
        };
        let loaded = match result {
            Ok(image) => {
                let bytes = Self::bytes(&image);
                if bytes > self.max_bytes
                    || !self.evict_until_fit(bytes, 0)
                    || !self.allocate_shared_bytes(bytes)
                {
                    self.stats.rejections = self.stats.rejections.saturating_add(1);
                    cx.drop_image(image, None);
                    self.stats.retirements = self.stats.retirements.saturating_add(1);
                    // Remember the rejection like a failed load: dropping the
                    // entry made the waiters' next render start the same
                    // download again, looping until the budget freed up.
                    if !self.reserve_failure_slot() {
                        self.entries.remove(&resource);
                        Self::notify(cx, waiters);
                        self.notify_slot_waiters(cx);
                        self.refresh();
                        return;
                    }
                    if bytes <= self.max_bytes {
                        Self::notify_after_failure_ttl(cx, waiters.clone());
                    }
                    (
                        Err(ImageCacheError::Asset(
                            "image does not fit the media cache budget".into(),
                        )),
                        0,
                    )
                } else {
                    (Ok(image), bytes)
                }
            }
            Err(error) => {
                self.stats.failures = self.stats.failures.saturating_add(1);
                if !self.reserve_failure_slot() {
                    self.entries.remove(&resource);
                    self.stats.rejections = self.stats.rejections.saturating_add(1);
                    self.refresh();
                    Self::notify(cx, waiters);
                    self.notify_slot_waiters(cx);
                    return;
                }
                (Err(error), 0)
            }
        };
        let last_used = self.tick();
        self.entries.insert(
            resource,
            Entry::Loaded {
                result: loaded.0,
                bytes: loaded.1,
                last_used,
                completed: Instant::now(),
                charged: loaded.1,
            },
        );
        self.trim_failures();
        self.refresh();
        Self::notify(cx, waiters);
        self.notify_slot_waiters(cx);
    }
}

impl ImageCache for EchoUriImageCache {
    fn load(
        &mut self,
        resource: &Resource,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<Result<Arc<RenderImage>, ImageCacheError>> {
        let waiter = window.current_view();
        if !self.mark_visible(window.window_handle().window_id(), resource) {
            self.queue_slot_waiter(waiter);
            self.stats.rejections = self.stats.rejections.saturating_add(1);
            self.schedule_one_shot_retry(waiter, window, cx);
            self.refresh();
            return None;
        }
        self.slot_waiters.remove(&waiter);
        self.retry_scheduled.remove(&waiter);
        let now = self.tick();
        if let Some(entry) = self.entries.get_mut(resource) {
            match entry {
                Entry::Loading { waiters, .. } => {
                    if waiters.len() < MAX_WAITERS_PER_RESOURCE || waiters.contains(&waiter) {
                        waiters.insert(waiter);
                    } else {
                        self.stats.waiter_rejections =
                            self.stats.waiter_rejections.saturating_add(1);
                    }
                    return None;
                }
                Entry::Loaded {
                    result,
                    last_used,
                    completed,
                    ..
                } if result.is_ok() || completed.elapsed() < FAILURE_TTL => {
                    *last_used = now;
                    let cached = result.clone();
                    self.stats.hits = self.stats.hits.saturating_add(1);
                    *DIAGNOSTIC_STATS.write() = self.stats;
                    return Some(cached);
                }
                Entry::Loaded { .. } => {
                    self.remove_entry(resource);
                }
            }
        }
        self.refresh();
        if self.stats.inflight >= MAX_INFLIGHT || !self.evict_until_fit(0, 1) {
            self.queue_slot_waiter(waiter);
            self.stats.rejections = self.stats.rejections.saturating_add(1);
            self.publish_stats();
            self.schedule_one_shot_retry(waiter, window, cx);
            return None;
        }
        self.stats.misses = self.stats.misses.saturating_add(1);
        self.retry_scheduled.remove(&waiter);
        let token = self.next_token;
        self.next_token = self.next_token.wrapping_add(1);
        let load = AssetLogger::<ImageAssetLoader>::load(resource.clone(), cx);
        let task = cx.background_executor().spawn(load).shared();
        self.entries.insert(
            resource.clone(),
            Entry::Loading {
                token,
                waiters: HashSet::from([waiter]),
            },
        );
        self.refresh();
        let cache = shared(cx);
        let resource = resource.clone();
        cx.spawn(async move |cx| {
            let result = task.await;
            cx.update(|cx| {
                cache.update(cx, |cache, cx| cache.complete(resource, token, result, cx));
            });
        })
        .detach();
        None
    }
}

/// State retained by GPUI across the root's request-layout and paint phases.
pub struct EchoUriImageCacheProvider {
    cache: Entity<EchoUriImageCache>,
    generation_started: bool,
}
impl ImageCacheProvider for EchoUriImageCacheProvider {
    fn provide(&mut self, window: &mut Window, cx: &mut App) -> gpui::AnyImageCache {
        if !self.generation_started {
            let id = window.window_handle().window_id();
            self.cache.update(cx, |cache, _| cache.begin_generation(id));
            self.generation_started = true;
        }
        self.cache.clone().into()
    }
}

struct UriImageCacheGlobal {
    cache: Entity<EchoUriImageCache>,
    _window_closed: gpui::Subscription,
}
impl Global for UriImageCacheGlobal {}

/// Install once before the first Echo window is opened.
pub fn init(cx: &mut App) {
    let cache = cx.new(|_| EchoUriImageCache::new());
    let close_cache = cache.clone();
    let window_closed = cx.on_window_closed(move |cx, window_id| {
        let waiters = close_cache.update(cx, |cache, _| cache.remove_window(window_id));
        for waiter in waiters {
            cx.notify(waiter);
        }
    });
    cx.set_global(UriImageCacheGlobal {
        cache,
        _window_closed: window_closed,
    });
}
fn shared(cx: &App) -> Entity<EchoUriImageCache> {
    cx.global::<UriImageCacheGlobal>().cache.clone()
}
pub fn provider(cx: &App) -> EchoUriImageCacheProvider {
    EchoUriImageCacheProvider {
        cache: shared(cx),
        generation_started: false,
    }
}
pub fn stats() -> UriImageCacheStats {
    *DIAGNOSTIC_STATS.read()
}
