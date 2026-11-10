//! User pronouns fetching and caching.
//!
//! Fetches pronouns from alejo.io API with 30-minute TTL caching.

use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use super::lock_utils::MutexExt;

/// Pronoun information with cache metadata
#[derive(Debug, Clone)]
pub struct PronounInfo {
    pub pronouns: String,
    pub fetched_at: Instant,
}

impl PronounInfo {
    /// Check if the cached pronouns are still valid (< 30 minutes old)
    fn is_valid(&self) -> bool {
        self.fetched_at.elapsed().as_secs() < 1800
    }
}

/// Cache for pronoun data
static PRONOUN_CACHE: LazyLock<Mutex<HashMap<String, PronounInfo>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
static PRONOUN_FETCHES_IN_FLIGHT: LazyLock<Mutex<HashSet<String>>> =
    LazyLock::new(|| Mutex::new(HashSet::new()));
/// Recent lookups that found nothing or failed, so a user without pronouns (or
/// an API outage) is not re-requested for every message. Tagged with the proxy
/// generation: a routing change clears it, since failures may have been the proxy.
static PRONOUN_MISSES: LazyLock<Mutex<(u64, HashMap<String, Instant>)>> =
    LazyLock::new(|| Mutex::new((crate::backend::http::proxy_generation(), HashMap::new())));
const PRONOUN_MISS_TTL: Duration = Duration::from_secs(5 * 60);
const PRONOUN_MISS_CAPACITY: usize = 4096;

fn lock_misses() -> std::sync::MutexGuard<'static, (u64, HashMap<String, Instant>)> {
    let mut misses = PRONOUN_MISSES.lock_or_recover();
    let generation = crate::backend::http::proxy_generation();
    if misses.0 != generation {
        *misses = (generation, HashMap::new());
    }
    misses
}

fn recently_missed(username_lower: &str) -> bool {
    lock_misses()
        .1
        .get(username_lower)
        .is_some_and(|at| at.elapsed() < PRONOUN_MISS_TTL)
}

fn record_miss(username_lower: String) {
    let mut guard = lock_misses();
    let misses = &mut guard.1;
    if misses.len() >= PRONOUN_MISS_CAPACITY {
        misses.retain(|_, at| at.elapsed() < PRONOUN_MISS_TTL);
        if misses.len() >= PRONOUN_MISS_CAPACITY {
            misses.clear();
        }
    }
    misses.insert(username_lower, Instant::now());
}

static PRONOUN_HTTP_CLIENT: LazyLock<crate::backend::http::ClientCache> =
    LazyLock::new(|| crate::backend::http::ClientCache::new(build_pronoun_http_client));

fn build_pronoun_http_client() -> reqwest::Client {
    crate::backend::http::timed_client(std::time::Duration::from_secs(10))
}

/// Get cached pronouns for a user (returns None if not cached or expired)
pub fn get_pronouns(username: &str) -> Option<String> {
    let cache = PRONOUN_CACHE.lock_or_recover();
    cache
        .get(&username.to_lowercase())
        .filter(|info| info.is_valid())
        .map(|info| info.pronouns.clone())
}

/// Queue a background pronoun fetch for a user if needed.
pub fn queue_fetch_pronouns(username: &str) {
    let username = username.trim().trim_start_matches('@').to_lowercase();
    if username.is_empty() || get_pronouns(&username).is_some() || recently_missed(&username) {
        return;
    }

    {
        let mut in_flight = PRONOUN_FETCHES_IN_FLIGHT.lock_or_recover();
        if !in_flight.insert(username.clone()) {
            return;
        }
    }

    crate::backend::bridge::get_runtime().spawn(async move {
        let _ = fetch_pronouns(&username).await;
        PRONOUN_FETCHES_IN_FLIGHT
            .lock_or_recover()
            .remove(&username);
    });
}

/// Fetch pronouns from alejo.io API and cache the result
pub async fn fetch_pronouns(username: &str) -> Option<String> {
    let username_lower = username.to_lowercase();

    {
        let cache = PRONOUN_CACHE.lock_or_recover();
        if let Some(info) = cache.get(&username_lower)
            && info.is_valid()
        {
            return Some(info.pronouns.clone());
        }
    }

    if recently_missed(&username_lower) {
        return None;
    }

    let url = format!("https://api.pronouns.alejo.io/v1/users/{}", username_lower);

    match PRONOUN_HTTP_CLIENT.get(&url).send().await {
        Ok(response) => {
            if response.status().is_success()
                && let Ok(data) = response.json::<serde_json::Value>().await
                && let Some(pronoun_id) = data.get("pronoun_id").and_then(|v| v.as_str())
            {
                let pronouns = format_pronoun_id(pronoun_id);

                let mut cache = PRONOUN_CACHE.lock_or_recover();
                cache.insert(
                    username_lower,
                    PronounInfo {
                        pronouns: pronouns.clone(),
                        fetched_at: Instant::now(),
                    },
                );

                return Some(pronouns);
            }
        }
        Err(e) => {
            crate::log_net!(
                debug,
                "Failed to fetch pronouns for {}: {}",
                username,
                e.without_url()
            );
        }
    }

    record_miss(username_lower);
    None
}

fn format_pronoun_id(pronoun_id: &str) -> String {
    match pronoun_id {
        "aeaer" => "ae/aer",
        "eem" => "e/em",
        "faefaer" => "fae/faer",
        "hehim" => "he/him",
        "heshe" => "he/she",
        "hethem" => "he/they",
        "itits" => "it/its",
        "perper" => "per/per",
        "sheher" => "she/her",
        "shethem" => "she/they",
        "theythem" => "they/them",
        "vever" => "ve/ver",
        "xem" => "xe/xem",
        "ziehir" => "zie/hir",
        "any" => "any",
        "ask" => "ask",
        "avoid" => "avoid pronouns",
        "other" => "other",
        _ => pronoun_id,
    }
    .to_string()
}
