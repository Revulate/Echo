//! Terminal embed metadata cache and bounded resource-flight registry.
//!
//! Metadata belongs to a canonical resource. Rendered message occurrences are
//! only subscribers while that resource is resolving; they never leak into the
//! 500-entry LRU cache.

mod benchmark_hooks;

use lru::LruCache;
use std::collections::{HashMap, HashSet, VecDeque};
use std::num::NonZeroUsize;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use crate::backend::embeds::state::PreviewOccurrence;
use crate::backend::link_types::{EmbedLoadState, EmbedMetadata, ResourceKey};
use crate::backend::lock_utils::MutexExt;

const MAX_CACHE_SIZE: usize = 500;
const CACHE_TTL_SUCCESS: Duration = Duration::from_secs(3600);
const CACHE_TTL_FAILURE: Duration = Duration::from_secs(300);
pub const MAX_ACTIVE_RESOURCE_FLIGHTS: usize = 64;
pub const MAX_DEFERRED_RESOURCE_FLIGHTS: usize = 128;
pub const MAX_FLIGHT_SUBSCRIBERS: usize = 256;

#[derive(Clone)]
struct CachedEmbed {
    metadata: EmbedMetadata,
    cached_at: Instant,
    policy_generation: u64,
    proxy_generation: u64,
    origin: CacheOrigin,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum CacheOrigin {
    Local,
    Remote,
}

impl CachedEmbed {
    fn is_expired(&self) -> bool {
        self.cached_at.elapsed()
            > match self.metadata.state {
                EmbedLoadState::Failed => CACHE_TTL_FAILURE,
                EmbedLoadState::Pending | EmbedLoadState::Loaded => CACHE_TTL_SUCCESS,
            }
    }
}

pub static EMBED_CACHE: LazyLock<EmbedCache> = LazyLock::new(EmbedCache::new);
pub static RESOURCE_FLIGHTS: LazyLock<ResourceFlightRegistry> =
    LazyLock::new(ResourceFlightRegistry::new);

pub struct EmbedCache {
    entries: Mutex<LruCache<String, CachedEmbed>>,
}

impl EmbedCache {
    pub fn new() -> Self {
        Self {
            entries: Mutex::new(LruCache::new(NonZeroUsize::new(MAX_CACHE_SIZE).unwrap())),
        }
    }

    /// Policy is deliberately checked again on cache reads: a metadata entry
    /// must not survive a provider/content policy change merely because it has
    /// not yet reached its TTL.
    pub fn get(&self, url: &str) -> Option<EmbedMetadata> {
        let classified = crate::backend::link_types::classify_link(url);
        if !crate::backend::settings::is_embed_classification_enabled(
            classified.provider,
            classified.content_kind,
        ) {
            return None;
        }
        let mut entries = self.entries.lock_or_recover();
        let entry = entries.get(&classified.resource_key.0)?.clone();
        if entry.is_expired()
            || entry.policy_generation != RESOURCE_FLIGHTS.policy_generation.load(Ordering::Acquire)
            || entry.proxy_generation != crate::backend::http::proxy_generation()
            || (entry.origin == CacheOrigin::Remote
                && !crate::backend::settings::get_embed_remote_fetch_enabled())
        {
            let _ = entries.pop(&classified.resource_key.0);
            return None;
        }
        Some(entry.metadata)
    }

    /// The registry calls this while holding its flight lock after it has
    /// verified that the work still owns the active resource slot. Keeping the
    /// cache commit inside that ownership boundary prevents a final-subscriber
    /// cancellation from racing a stale task's terminal publication.
    fn insert_current(&self, work: &FlightWork, metadata: EmbedMetadata) {
        let origin = if crate::backend::settings::get_embed_remote_fetch_enabled() {
            CacheOrigin::Remote
        } else {
            CacheOrigin::Local
        };
        // Keyed like `get`. A flight carries its occurrence's URL key, which
        // differs from a provider link's canonical key (`twitch:channel:…`);
        // stored under the URL key, such a link never hit the cache and was
        // fetched again on every visible-range scan.
        let resource_key = crate::backend::link_types::classify_link(&work.url).resource_key;
        self.entries.lock_or_recover().put(
            resource_key.0,
            CachedEmbed {
                metadata,
                cached_at: Instant::now(),
                policy_generation: work.policy_generation,
                proxy_generation: work.proxy_generation,
                origin,
            },
        );
    }

    pub fn clear(&self) {
        self.entries.lock_or_recover().clear();
    }
}

impl Default for EmbedCache {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlightDemand {
    Always,
    Manual,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlightAdmissionKind {
    Started,
    Joined,
    Deferred,
    RejectedCapacity,
}

#[derive(Debug, Clone)]
pub struct FlightWork {
    pub resource_key: ResourceKey,
    pub url: String,
    id: u64,
    policy_generation: u64,
    proxy_generation: u64,
}

impl FlightWork {
    pub fn is_current(&self) -> bool {
        RESOURCE_FLIGHTS.is_current(self)
    }
}

#[derive(Debug, Clone)]
pub struct FlightAdmission {
    pub kind: FlightAdmissionKind,
    pub work: Option<FlightWork>,
}

#[derive(Debug)]
pub struct FlightCompletion {
    pub subscribers: Vec<PreviewOccurrence>,
    pub overflowed: bool,
    pub stale: bool,
    pub next: Vec<FlightWork>,
}

#[derive(Debug)]
struct Flight {
    work: FlightWork,
    subscribers: HashSet<PreviewOccurrence>,
    overflowed: bool,
}

#[derive(Debug)]
struct DeferredFlight {
    url: String,
    demand: FlightDemand,
    subscribers: HashSet<PreviewOccurrence>,
    overflowed: bool,
}

#[derive(Default)]
struct Flights {
    active: HashMap<ResourceKey, Flight>,
    deferred: HashMap<ResourceKey, DeferredFlight>,
    manual_queue: VecDeque<ResourceKey>,
    always_queue: VecDeque<ResourceKey>,
}

pub struct ResourceFlightRegistry {
    flights: Mutex<Flights>,
    policy_generation: AtomicU64,
    next_id: AtomicU64,
}

impl ResourceFlightRegistry {
    pub fn new() -> Self {
        Self {
            flights: Mutex::new(Flights::default()),
            policy_generation: AtomicU64::new(0),
            next_id: AtomicU64::new(1),
        }
    }

    pub fn request(
        &self,
        resource_key: ResourceKey,
        url: String,
        occurrence: PreviewOccurrence,
        demand: FlightDemand,
    ) -> FlightAdmission {
        let mut flights = self.flights.lock_or_recover();
        if let Some(flight) = flights.active.get_mut(&resource_key) {
            Self::subscribe(&mut flight.subscribers, &mut flight.overflowed, occurrence);
            return FlightAdmission {
                kind: FlightAdmissionKind::Joined,
                work: None,
            };
        }
        if let Some(deferred) = flights.deferred.get_mut(&resource_key) {
            Self::subscribe(
                &mut deferred.subscribers,
                &mut deferred.overflowed,
                occurrence,
            );
            if demand == FlightDemand::Manual && deferred.demand == FlightDemand::Always {
                deferred.demand = FlightDemand::Manual;
                flights.manual_queue.push_back(resource_key);
            }
            return FlightAdmission {
                kind: FlightAdmissionKind::Deferred,
                work: None,
            };
        }
        if flights.active.len() < MAX_ACTIVE_RESOURCE_FLIGHTS {
            let work =
                self.start_locked(&mut flights, resource_key, url, occurrence, demand, false);
            return FlightAdmission {
                kind: FlightAdmissionKind::Started,
                work: Some(work),
            };
        }
        if flights.deferred.len() >= MAX_DEFERRED_RESOURCE_FLIGHTS {
            return FlightAdmission {
                kind: FlightAdmissionKind::RejectedCapacity,
                work: None,
            };
        }
        let mut subscribers = HashSet::new();
        subscribers.insert(occurrence);
        flights.deferred.insert(
            resource_key.clone(),
            DeferredFlight {
                url,
                demand,
                subscribers,
                overflowed: false,
            },
        );
        match demand {
            FlightDemand::Manual => flights.manual_queue.push_back(resource_key),
            FlightDemand::Always => flights.always_queue.push_back(resource_key),
        }
        FlightAdmission {
            kind: FlightAdmissionKind::Deferred,
            work: None,
        }
    }

    pub fn finish(&self, work: &FlightWork) -> FlightCompletion {
        let mut flights = self.flights.lock_or_recover();
        let Some(flight) = self.take_current_locked(&mut flights, work) else {
            return Self::stale_completion();
        };
        let next = self.drain_locked(&mut flights);
        FlightCompletion {
            subscribers: flight.subscribers.into_iter().collect(),
            overflowed: flight.overflowed,
            stale: false,
            next,
        }
    }

    /// Atomically verify the active-flight owner and commit its terminal
    /// metadata before releasing that ownership. A cancellation that wins the
    /// registry lock removes the flight first; a completion that wins commits
    /// before the cancellation observes it as no longer running.
    pub fn complete(&self, work: &FlightWork, metadata: EmbedMetadata) -> FlightCompletion {
        let mut flights = self.flights.lock_or_recover();
        let Some(flight) = self.take_current_locked(&mut flights, work) else {
            return Self::stale_completion();
        };
        EMBED_CACHE.insert_current(work, metadata);
        let next = self.drain_locked(&mut flights);
        FlightCompletion {
            subscribers: flight.subscribers.into_iter().collect(),
            overflowed: flight.overflowed,
            stale: false,
            next,
        }
    }

    pub fn cancel_occurrence(&self, occurrence: &PreviewOccurrence) -> Vec<FlightWork> {
        let mut flights = self.flights.lock_or_recover();
        let key = occurrence.resource_key.clone();
        if let Some(flight) = flights.active.get_mut(&key) {
            flight.subscribers.remove(occurrence);
            if flight.subscribers.is_empty() && !flight.overflowed {
                flights.active.remove(&key);
            }
        }
        if let Some(deferred) = flights.deferred.get_mut(&key) {
            deferred.subscribers.remove(occurrence);
            if deferred.subscribers.is_empty() && !deferred.overflowed {
                flights.deferred.remove(&key);
            }
        }
        self.drain_locked(&mut flights)
    }

    /// Invalidates all cache/flight state on a policy transition. Running tasks
    /// observe their captured generation as stale and must publish nothing.
    pub fn invalidate_policy(&self) {
        self.policy_generation.fetch_add(1, Ordering::AcqRel);
        let mut flights = self.flights.lock_or_recover();
        flights.active.clear();
        flights.deferred.clear();
        flights.manual_queue.clear();
        flights.always_queue.clear();
    }

    /// Cancel active/deferred ownership after a transport-routing change while
    /// retaining terminal metadata: proxy changes do not make a completed
    /// resource description false, only in-flight I/O unsafe to publish.
    pub fn invalidate_flights(&self) {
        self.invalidate_policy();
    }

    fn is_current(&self, work: &FlightWork) -> bool {
        if !work.is_current_without_registry(self) {
            return false;
        }
        self.flights
            .lock_or_recover()
            .active
            .get(&work.resource_key)
            .is_some_and(|flight| {
                flight.work.id == work.id && work.is_current_without_registry(self)
            })
    }

    fn stale_completion() -> FlightCompletion {
        FlightCompletion {
            subscribers: Vec::new(),
            overflowed: false,
            stale: true,
            next: Vec::new(),
        }
    }

    fn take_current_locked(&self, flights: &mut Flights, work: &FlightWork) -> Option<Flight> {
        if !work.is_current_without_registry(self)
            || flights
                .active
                .get(&work.resource_key)
                .is_none_or(|flight| flight.work.id != work.id)
        {
            return None;
        }
        flights.active.remove(&work.resource_key)
    }

    fn subscribe(
        subscribers: &mut HashSet<PreviewOccurrence>,
        overflowed: &mut bool,
        occurrence: PreviewOccurrence,
    ) {
        if subscribers.contains(&occurrence) {
            return;
        }
        if subscribers.len() < MAX_FLIGHT_SUBSCRIBERS {
            subscribers.insert(occurrence);
        } else {
            *overflowed = true;
        }
    }

    fn start_locked(
        &self,
        flights: &mut Flights,
        resource_key: ResourceKey,
        url: String,
        occurrence: PreviewOccurrence,
        _demand: FlightDemand,
        overflowed: bool,
    ) -> FlightWork {
        let work = FlightWork {
            resource_key: resource_key.clone(),
            url,
            id: self.next_id.fetch_add(1, Ordering::Relaxed),
            policy_generation: self.policy_generation.load(Ordering::Acquire),
            proxy_generation: crate::backend::http::proxy_generation(),
        };
        let mut subscribers = HashSet::new();
        subscribers.insert(occurrence);
        flights.active.insert(
            resource_key,
            Flight {
                work: work.clone(),
                subscribers,
                overflowed,
            },
        );
        work
    }

    fn drain_locked(&self, flights: &mut Flights) -> Vec<FlightWork> {
        let mut next = Vec::new();
        while flights.active.len() < MAX_ACTIVE_RESOURCE_FLIGHTS {
            let key = Self::pop_next_key(flights);
            let Some(key) = key else { break };
            let Some(deferred) = flights.deferred.remove(&key) else {
                continue;
            };
            let Some(occurrence) = deferred.subscribers.iter().next().cloned() else {
                continue;
            };
            let work = self.start_locked(
                flights,
                key,
                deferred.url,
                occurrence.clone(),
                deferred.demand,
                deferred.overflowed,
            );
            if let Some(flight) = flights.active.get_mut(&work.resource_key) {
                flight.subscribers = deferred.subscribers;
            }
            next.push(work);
        }
        next
    }

    fn pop_next_key(flights: &mut Flights) -> Option<ResourceKey> {
        while let Some(key) = flights.manual_queue.pop_front() {
            if flights
                .deferred
                .get(&key)
                .is_some_and(|flight| flight.demand == FlightDemand::Manual)
            {
                return Some(key);
            }
        }
        while let Some(key) = flights.always_queue.pop_front() {
            if flights
                .deferred
                .get(&key)
                .is_some_and(|flight| flight.demand == FlightDemand::Always)
            {
                return Some(key);
            }
        }
        None
    }
}

impl FlightWork {
    fn is_current_without_registry(&self, registry: &ResourceFlightRegistry) -> bool {
        self.policy_generation == registry.policy_generation.load(Ordering::Acquire)
            && self.proxy_generation == crate::backend::http::proxy_generation()
    }
}

impl Default for ResourceFlightRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::link_types::classify_link;
    use core::prelude::v1::test;

    #[test]
    fn completed_provider_link_is_found_by_get() {
        let url = "https://twitch.tv/finalfantasyxiv";
        let occurrence_key = ResourceKey::for_url(url);
        assert_ne!(occurrence_key, classify_link(url).resource_key);

        let cache = EmbedCache::new();
        let work = FlightWork {
            resource_key: occurrence_key,
            url: url.to_string(),
            id: 1,
            policy_generation: RESOURCE_FLIGHTS.policy_generation.load(Ordering::Acquire),
            proxy_generation: crate::backend::http::proxy_generation(),
        };
        cache.insert_current(
            &work,
            crate::backend::embeds::fetchers::failed_embed_metadata(url),
        );
        assert!(cache.get(url).is_some());
    }
}
