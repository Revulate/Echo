//! Retry utilities using backon for resilient HTTP requests
//!
//! Provides helper functions for retrying transient network errors with
//! exponential backoff and jitter.
//!
//! Usage pattern:
//! ```rust,no_run
//! use backon::Retryable;
//! use echo_gpui::backend::retry::{default_backoff, is_reqwest_transient};
//!
//! # async fn fetch() -> Result<(), reqwest::Error> {
//! let client = reqwest::Client::new();
//! let url = "https://example.com";
//! let _resp = (|| {
//!     let client = client.clone();
//!     async move { client.get(url).send().await }
//! })
//! .retry(default_backoff())
//! .when(is_reqwest_transient)
//! .await?;
//! # Ok(())
//! # }
//! ```

use backon::ExponentialBuilder;
use std::time::Duration;

pub trait RetryHttpClient {
    fn retry_client(&self) -> reqwest::Client;
}

impl RetryHttpClient for reqwest::Client {
    fn retry_client(&self) -> reqwest::Client {
        self.clone()
    }
}

impl RetryHttpClient for crate::backend::http::ClientCache {
    fn retry_client(&self) -> reqwest::Client {
        self.client()
    }
}

impl RetryHttpClient for std::sync::LazyLock<crate::backend::http::ClientCache> {
    fn retry_client(&self) -> reqwest::Client {
        self.client()
    }
}

/// Default retry configuration for HTTP requests
/// - Min delay: 100ms
/// - Max delay: 5s
/// - Max attempts: 3
/// - Jitter enabled for avoiding thundering herd
pub fn default_backoff() -> ExponentialBuilder {
    ExponentialBuilder::default()
        .with_min_delay(Duration::from_millis(100))
        .with_max_delay(Duration::from_secs(5))
        .with_max_times(3)
        .with_jitter()
}

/// Spread a reconnect delay by up to 20% either way (clock-seeded, like the IRC
/// backoff) so clients that lost a provider together do not reconnect in lockstep.
pub fn with_reconnect_jitter(delay: Duration) -> Duration {
    let millis = u64::try_from(delay.as_millis()).unwrap_or(u64::MAX);
    let spread = millis / 5;
    if spread == 0 {
        return delay;
    }
    let seed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_nanos() as u64;
    let offset = seed % (spread * 2 + 1);
    Duration::from_millis(millis.saturating_add(offset).saturating_sub(spread))
}

/// Whether a failed request is worth retrying: it never connected, timed out,
/// got 429/502/503/504, or its connection dropped mid-exchange.
pub fn is_reqwest_transient(err: &reqwest::Error) -> bool {
    if err.is_connect() || err.is_timeout() {
        return true;
    }

    if let Some(status) = err.status() {
        let code = status.as_u16();
        return code == 429 || code == 502 || code == 503 || code == 504;
    }

    io_error_kinds(err).any(|kind| {
        matches!(
            kind,
            std::io::ErrorKind::ConnectionReset
                | std::io::ErrorKind::ConnectionAborted
                | std::io::ErrorKind::ConnectionRefused
                | std::io::ErrorKind::BrokenPipe
                | std::io::ErrorKind::TimedOut
                | std::io::ErrorKind::UnexpectedEof
        )
    })
}

/// The kinds of every I/O error in `err`'s source chain.
fn io_error_kinds(err: &reqwest::Error) -> impl Iterator<Item = std::io::ErrorKind> + '_ {
    std::iter::successors(std::error::Error::source(err), |source| source.source())
        .filter_map(|source| source.downcast_ref::<std::io::Error>())
        .map(std::io::Error::kind)
}

/// Server-requested wait cap for emote provider fetches. Channel history waits
/// on emotes, so a longer `Retry-After` gives up instead of stalling the channel.
pub const EMOTE_MAX_RETRY_AFTER: Duration = Duration::from_secs(5);

/// Provider rate limiter re-acquired before each retry attempt.
pub type ProviderLimiter = governor::RateLimiter<
    governor::state::NotKeyed,
    governor::state::InMemoryState,
    governor::clock::DefaultClock,
    governor::middleware::NoOpMiddleware,
>;

/// Statuses that mean "try again shortly": rate limiting and gateway/overload.
/// `send()` returns these as `Ok(Response)`, so they are checked separately
/// from transport errors.
pub fn is_retryable_status(status: reqwest::StatusCode) -> bool {
    matches!(status.as_u16(), 429 | 502 | 503 | 504)
}

/// Parse a `Retry-After` header given in delta-seconds. HTTP-date values are
/// ignored (the backoff delay is used).
pub fn parse_retry_after(value: &str) -> Option<Duration> {
    let secs = value.trim().parse::<u64>().ok()?;
    Some(Duration::from_secs(secs))
}

/// Parse a Helix `Ratelimit-Reset` header (Unix epoch seconds when the bucket
/// refills) into the wait from `now_epoch_secs`, clamped at zero.
pub fn parse_ratelimit_reset(value: &str, now_epoch_secs: u64) -> Option<Duration> {
    let reset = value.trim().parse::<u64>().ok()?;
    Some(Duration::from_secs(reset.saturating_sub(now_epoch_secs)))
}

/// Server-requested wait before retrying: `Retry-After`, else Helix
/// `Ratelimit-Reset` (epoch seconds).
pub fn server_retry_wait(headers: &reqwest::header::HeaderMap) -> Option<Duration> {
    let header = |name: &str| headers.get(name).and_then(|v| v.to_str().ok());
    header(reqwest::header::RETRY_AFTER.as_str())
        .and_then(parse_retry_after)
        .or_else(|| {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            header("ratelimit-reset").and_then(|v| parse_ratelimit_reset(v, now))
        })
}

/// Delay before retrying after a server-requested `wait`: never earlier than
/// `wait` or the backoff step, plus up to half a backoff step of jitter so
/// throttled clients do not retry in lockstep, and never beyond `cap` (callers
/// give up instead when `wait` itself exceeds `cap`).
pub fn server_wait_delay(wait: Duration, backoff: Duration, cap: Duration, seed: u64) -> Duration {
    let spread = u64::try_from(backoff.as_millis() / 2).unwrap_or(u64::MAX);
    let jitter = Duration::from_millis(seed % spread.saturating_add(1));
    wait.max(backoff).saturating_add(jitter).min(cap.max(wait))
}

/// Run `send` with exponential backoff, retrying transient transport errors and
/// 429/502/503/504 responses (honouring `Retry-After` / `Ratelimit-Reset`).
/// A server-requested wait longer than `max_server_wait` gives up at once.
/// `limiter`, when set, is re-acquired before each retry. Any other response is
/// returned unchanged; when retries run out the last response or error is
/// returned so the caller still sees the real status. The final attempt is
/// logged through [`crate::backend::http::log_http_outcome`] (all callers are
/// emote provider GETs); a retried attempt only logs its debug retry line, so
/// one failing fetch warns once.
async fn send_with_retry<F, Fut>(
    url: &str,
    max_server_wait: Duration,
    limiter: Option<&ProviderLimiter>,
    mut send: F,
) -> Result<reqwest::Response, reqwest::Error>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<reqwest::Response, reqwest::Error>>,
{
    use backon::BackoffBuilder;

    let mut backoff = default_backoff().build();
    let mut attempt = 1u32;
    loop {
        let started = std::time::Instant::now();
        let result = send().await;
        let elapsed = started.elapsed();
        let finish = |result: Result<reqwest::Response, reqwest::Error>| {
            crate::backend::http::log_http_outcome(
                crate::backend::http::LogCategory::Emotes,
                "GET",
                url,
                result.as_ref().map(reqwest::Response::status),
                elapsed,
                attempt,
            );
            result
        };
        let (delay, reason) = match &result {
            Ok(resp) if is_retryable_status(resp.status()) => {
                let Some(delay) = backoff.next() else {
                    crate::log_net!(
                        debug,
                        "Giving up on {} after {} attempts: HTTP {}",
                        crate::log::redact_url(url),
                        attempt,
                        resp.status()
                    );
                    return finish(result);
                };
                let delay = match server_retry_wait(resp.headers()) {
                    Some(wait) if wait > max_server_wait => {
                        crate::log_net!(
                            debug,
                            "Giving up on {}: HTTP {} asks to wait {:?} (cap {:?})",
                            crate::log::redact_url(url),
                            resp.status(),
                            wait,
                            max_server_wait
                        );
                        return finish(result);
                    }
                    Some(wait) => {
                        let seed = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap_or_default()
                            .subsec_nanos();
                        server_wait_delay(wait, delay, max_server_wait, u64::from(seed))
                    }
                    None => delay,
                };
                (delay, format!("HTTP {}", resp.status()))
            }
            Err(err) if is_reqwest_transient(err) => {
                let Some(delay) = backoff.next() else {
                    crate::log_net!(
                        debug,
                        "Giving up on {} after {} attempts: {}",
                        crate::log::redact_url(url),
                        attempt,
                        crate::backend::http::error_kind(err)
                    );
                    return finish(result);
                };
                (delay, crate::backend::http::error_kind(err).to_string())
            }
            _ => return finish(result),
        };
        crate::log_net!(
            debug,
            "Retrying {} in {:?} (attempt {} failed: {})",
            crate::log::redact_url(url),
            delay,
            attempt,
            reason
        );
        drop(result);
        tokio::time::sleep(delay).await;
        if let Some(limiter) = limiter {
            limiter.until_ready().await;
        }
        attempt += 1;
    }
}

/// Execute an authenticated GET request to Twitch Helix API with retry.
///
/// This helper provides the standard Twitch API authentication pattern with
/// automatic retry for transient errors.
///
/// # Arguments
/// * `client` - The reqwest Client to use
/// * `url` - The Helix API URL to fetch
/// * `client_id` - Twitch Client ID
/// * `token` - OAuth token (without "Bearer " prefix)
/// * `max_server_wait` - longest server-requested wait to honour before giving up
pub async fn retry_helix_get(
    client: &(impl RetryHttpClient + ?Sized),
    url: &str,
    client_id: &str,
    token: &str,
    max_server_wait: Duration,
) -> Result<reqwest::Response, reqwest::Error> {
    let client = client.retry_client();

    send_with_retry(url, max_server_wait, None, || {
        client
            .get(url)
            .header("Client-Id", client_id)
            .header("Authorization", format!("Bearer {}", token))
            .send()
    })
    .await
}

/// Execute an authenticated GET request to Twitch Helix API with retry.
///
/// Same as `retry_helix_get` but converts the error to a String.
pub async fn retry_helix_get_str(
    client: &(impl RetryHttpClient + ?Sized),
    url: &str,
    client_id: &str,
    token: &str,
    max_server_wait: Duration,
) -> Result<reqwest::Response, String> {
    retry_helix_get(client, url, client_id, token, max_server_wait)
        .await
        .map_err(|e| e.to_string())
}

/// Simple GET request with retry - minimal boilerplate version.
///
/// This is the simplest retry helper - just wraps a URL fetch with exponential backoff.
/// Use this for external APIs like 7TV, BTTV, FFZ.
///
/// # Arguments
/// * `client` - The reqwest Client to use
/// * `url` - The URL to fetch
/// * `max_server_wait` - longest server-requested wait to honour before giving up
/// * `limiter` - provider rate limiter to re-acquire before each retry
///
/// # Example
/// ```ignore
/// let resp = fetch_with_retry(&EMOTE_HTTP_CLIENT, &url, EMOTE_MAX_RETRY_AFTER, None).await?;
/// ```
pub async fn fetch_with_retry(
    client: &(impl RetryHttpClient + ?Sized),
    url: &str,
    max_server_wait: Duration,
    limiter: Option<&ProviderLimiter>,
) -> Result<reqwest::Response, reqwest::Error> {
    let client = client.retry_client();

    send_with_retry(url, max_server_wait, limiter, || client.get(url).send()).await
}

/// Simple GET request with retry, returning String error.
///
/// Same as `fetch_with_retry` but converts the error to a String for easier use
/// in contexts that need String errors.
pub async fn fetch_with_retry_str(
    client: &(impl RetryHttpClient + ?Sized),
    url: &str,
    max_server_wait: Duration,
    limiter: Option<&ProviderLimiter>,
) -> Result<reqwest::Response, String> {
    fetch_with_retry(client, url, max_server_wait, limiter)
        .await
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test;

    #[test]
    fn retryable_statuses() {
        for code in [429u16, 502, 503, 504] {
            assert!(is_retryable_status(
                reqwest::StatusCode::from_u16(code).unwrap()
            ));
        }
        for code in [200u16, 400, 401, 403, 404, 500] {
            assert!(!is_retryable_status(
                reqwest::StatusCode::from_u16(code).unwrap()
            ));
        }
    }

    #[test]
    fn retry_after_parsing() {
        assert_eq!(parse_retry_after("5"), Some(Duration::from_secs(5)));
        assert_eq!(parse_retry_after(" 0 "), Some(Duration::ZERO));
        assert_eq!(parse_retry_after("3600"), Some(Duration::from_secs(3600)));
        assert_eq!(parse_retry_after("Wed, 21 Oct 2015 07:28:00 GMT"), None);
        assert_eq!(parse_retry_after("-1"), None);
    }

    #[test]
    fn ratelimit_reset_is_epoch_relative() {
        assert_eq!(
            parse_ratelimit_reset("1700000012", 1_700_000_000),
            Some(Duration::from_secs(12))
        );
        assert_eq!(
            parse_ratelimit_reset(" 1700000000 ", 1_700_000_005),
            Some(Duration::ZERO)
        );
        assert_eq!(parse_ratelimit_reset("soon", 1_700_000_000), None);
    }

    #[test]
    fn server_wait_delay_jitters_within_cap() {
        let cap = Duration::from_secs(5);
        let backoff = Duration::from_millis(400);
        for seed in [0u64, 1, 199, 200, 12_345] {
            let delay = server_wait_delay(Duration::from_secs(2), backoff, cap, seed);
            assert!(delay >= Duration::from_secs(2));
            assert!(delay <= Duration::from_millis(2_200));
        }
        assert_eq!(
            server_wait_delay(Duration::ZERO, backoff, cap, 0),
            Duration::from_millis(400)
        );
        assert_eq!(
            server_wait_delay(Duration::from_millis(4_900), backoff, cap, 199),
            cap
        );
        assert_eq!(server_wait_delay(cap, Duration::from_secs(10), cap, 7), cap);
    }
}
