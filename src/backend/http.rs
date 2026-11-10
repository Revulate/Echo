//! Shared HTTP client helpers.

use crate::backend::lock_utils::MutexExt;
use crate::backend::lock_utils::RwLockExt;
use crate::backend::settings;
use std::collections::HashMap;
use std::sync::Once;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, RwLock};
use std::time::{Duration, Instant};

static CLIENT_GENERATION: AtomicU64 = AtomicU64::new(0);
static RUSTLS_PROVIDER: Once = Once::new();

/// Install Echo's single process-wide rustls crypto provider before a client
/// builder tries to create a TLS configuration.
pub fn ensure_rustls_crypto_provider() {
    RUSTLS_PROVIDER.call_once(|| {
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

/// Publishes the routing generation to clients that must be rebuilt eagerly
/// (GPUI's image client lives on the UI thread and cannot poll lazily).
static PROXY_CHANGES: std::sync::LazyLock<tokio::sync::watch::Sender<u64>> =
    std::sync::LazyLock::new(|| tokio::sync::watch::channel(0).0);

/// Mark configured network routing as changed so cached clients rebuild lazily.
pub fn mark_proxy_settings_changed() {
    let generation = CLIENT_GENERATION.fetch_add(1, Ordering::AcqRel) + 1;
    crate::backend::embeds::cache::RESOURCE_FLIGHTS.invalidate_flights();
    PROXY_CHANGES.send_replace(generation);
}

/// Resolves each time proxy routing changes (see [`mark_proxy_settings_changed`]).
pub fn subscribe_proxy_changes() -> tokio::sync::watch::Receiver<u64> {
    PROXY_CHANGES.subscribe()
}

/// Proxy for clients configured by URL rather than a reqwest builder (GPUI's
/// image client): `None` for direct routing, the configured proxy when it is
/// usable, and an unconnectable proxy when it is enabled but invalid, so those
/// requests fail closed like every other client.
pub fn effective_proxy_url() -> Option<url::Url> {
    let proxy_url = settings::get_proxy_url();
    let trimmed = proxy_url.trim();
    if !settings::get_proxy_enabled() || trimmed.is_empty() {
        return None;
    }
    let blocked = || url::Url::parse(BLOCKED_PROXY_URL).ok();
    if let Some(reason) = proxy_url_error(trimmed) {
        report_invalid_proxy(trimmed, &reason);
        return blocked();
    }
    url::Url::parse(trimmed).ok().or_else(blocked)
}

/// Current routing generation. Long-running callers capture this before an
/// operation and discard completion when proxy routing changed underneath it.
pub fn proxy_generation() -> u64 {
    CLIENT_GENERATION.load(Ordering::Acquire)
}

/// Lazily rebuilds a reqwest client when proxy settings change.
pub struct ClientCache {
    build: fn() -> reqwest::Client,
    inner: RwLock<(u64, reqwest::Client)>,
}

impl ClientCache {
    pub fn new(build: fn() -> reqwest::Client) -> Self {
        Self {
            build,
            inner: RwLock::new((proxy_generation(), build())),
        }
    }

    pub fn client(&self) -> reqwest::Client {
        let generation = proxy_generation();
        {
            let inner = self.inner.read_or_recover();
            if inner.0 == generation {
                return inner.1.clone();
            }
        }

        let mut inner = self.inner.write_or_recover();
        if inner.0 != generation {
            *inner = (generation, (self.build)());
        }
        inner.1.clone()
    }

    pub fn get<U: reqwest::IntoUrl>(&self, url: U) -> reqwest::RequestBuilder {
        self.client().get(url)
    }

    pub fn head<U: reqwest::IntoUrl>(&self, url: U) -> reqwest::RequestBuilder {
        self.client().head(url)
    }

    pub fn post<U: reqwest::IntoUrl>(&self, url: U) -> reqwest::RequestBuilder {
        self.client().post(url)
    }

    pub fn put<U: reqwest::IntoUrl>(&self, url: U) -> reqwest::RequestBuilder {
        self.client().put(url)
    }

    pub fn patch<U: reqwest::IntoUrl>(&self, url: U) -> reqwest::RequestBuilder {
        self.client().patch(url)
    }

    pub fn delete<U: reqwest::IntoUrl>(&self, url: U) -> reqwest::RequestBuilder {
        self.client().delete(url)
    }
}

/// Return a reqwest client builder with Echo's configured proxy applied.
pub fn client_builder() -> reqwest::ClientBuilder {
    ensure_rustls_crypto_provider();
    apply_proxy_settings(reqwest::Client::builder().user_agent(crate::identity::USER_AGENT))
}

/// Connect timeout for every client built here, so a black-holed route cannot
/// hang a request before the TCP/TLS handshake completes.
const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Unconnectable proxy (port 0 on loopback) used to fail closed: every request
/// errors locally instead of silently bypassing a configured proxy.
const BLOCKED_PROXY_URL: &str = "http://127.0.0.1:0";

/// Shown once when an enabled proxy URL cannot be used.
pub const INVALID_PROXY_MESSAGE: &str =
    "Proxy URL is invalid — network requests are blocked until it is fixed";

/// Invalid proxy URL last reported to the user, so the warning fires once per value.
static REPORTED_INVALID_PROXY: Mutex<Option<String>> = Mutex::new(None);

/// Build a default reqwest client with Echo's configured proxy applied. It has
/// a connect timeout but no total timeout (uploads can legitimately be long).
pub fn client() -> reqwest::Client {
    build_or_fallback(
        client_builder().connect_timeout(DEFAULT_CONNECT_TIMEOUT),
        None,
    )
}

/// Build a proxy-aware client whose requests give up after `timeout`.
pub fn timed_client(timeout: Duration) -> reqwest::Client {
    build_or_fallback(
        client_builder()
            .timeout(timeout)
            .connect_timeout(timeout.min(DEFAULT_CONNECT_TIMEOUT)),
        Some(timeout),
    )
}

fn build_or_fallback(
    builder: reqwest::ClientBuilder,
    timeout: Option<Duration>,
) -> reqwest::Client {
    builder.build().unwrap_or_else(|error| {
        tracing::error!(
            "Failed to create proxy-aware HTTP client: {}",
            error.without_url()
        );
        fallback_client(timeout)
    })
}

/// Last-resort client for when a configured builder fails. It keeps the
/// caller's timeout and never connects directly while a proxy is enabled.
fn fallback_client(timeout: Option<Duration>) -> reqwest::Client {
    ensure_rustls_crypto_provider();
    let mut builder = reqwest::Client::builder()
        .user_agent(crate::identity::USER_AGENT)
        .connect_timeout(DEFAULT_CONNECT_TIMEOUT);
    if let Some(timeout) = timeout {
        builder = builder.timeout(timeout);
    }
    if settings::get_proxy_enabled() && !settings::get_proxy_url().trim().is_empty() {
        builder = block_all_requests(builder);
    }
    builder.build().unwrap_or_default()
}

fn block_all_requests(builder: reqwest::ClientBuilder) -> reqwest::ClientBuilder {
    match reqwest::Proxy::all(BLOCKED_PROXY_URL) {
        Ok(proxy) => builder.proxy(proxy),
        // Unreachable for the constant above; still never connect directly.
        Err(_) => builder.no_proxy().connect_timeout(Duration::ZERO),
    }
}

/// Why the configured proxy cannot be used, or `None` when routing is direct
/// or the proxy URL is usable. Mirrors the checks WebSocket/IRC apply.
pub fn proxy_config_error() -> Option<String> {
    if !settings::get_proxy_enabled() {
        return None;
    }
    let proxy_url = settings::get_proxy_url();
    let trimmed = proxy_url.trim();
    if trimmed.is_empty() {
        return None;
    }
    proxy_url_error(trimmed)
}

fn proxy_url_error(proxy_url: &str) -> Option<String> {
    let parsed = match url::Url::parse(proxy_url) {
        Ok(parsed) => parsed,
        Err(error) => return Some(format!("Invalid proxy URL: {error}")),
    };
    if !matches!(parsed.scheme(), "http" | "https" | "socks5" | "socks5h") {
        return Some(format!("Unsupported proxy scheme '{}'", parsed.scheme()));
    }
    if parsed.host_str().is_none() {
        return Some("Proxy URL must include a host".to_string());
    }
    reqwest::Proxy::all(proxy_url)
        .err()
        .map(|error| format!("Invalid proxy URL: {}", error.without_url()))
}

/// Warn (log + toast) once per invalid proxy value.
fn report_invalid_proxy(proxy_url: &str, reason: &str) {
    {
        let mut reported = REPORTED_INVALID_PROXY.lock_or_recover();
        if reported.as_deref() == Some(proxy_url) {
            return;
        }
        *reported = Some(proxy_url.to_string());
    }
    crate::log_net!(
        warn,
        "Proxy URL {} is invalid ({}); blocking HTTP requests until it is fixed",
        crate::log::redact_url(proxy_url),
        reason
    );
    crate::backend::push_ui_action(crate::backend::UiAction::ShowWarningToast {
        message: INVALID_PROXY_MESSAGE.to_string(),
        detail: Some("Fix or clear it in Settings > Advanced > Network proxy.".to_string()),
    });
}

/// Apply the configured manual proxy to an existing reqwest client builder.
/// An enabled but invalid proxy fails closed: requests error instead of
/// connecting directly, matching the WebSocket and IRC paths.
pub fn apply_proxy_settings(builder: reqwest::ClientBuilder) -> reqwest::ClientBuilder {
    let proxy_url = settings::get_proxy_url();
    let trimmed = proxy_url.trim();
    if !settings::get_proxy_enabled() || trimmed.is_empty() {
        *REPORTED_INVALID_PROXY.lock_or_recover() = None;
        return builder;
    }

    if let Some(reason) = proxy_url_error(trimmed) {
        report_invalid_proxy(trimmed, &reason);
        return block_all_requests(builder);
    }
    *REPORTED_INVALID_PROXY.lock_or_recover() = None;
    match reqwest::Proxy::all(trimmed) {
        Ok(proxy) => builder.proxy(proxy),
        Err(_) => block_all_requests(builder),
    }
}

/// Log category an HTTP boundary line is written under.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LogCategory {
    Net,
    Emotes,
    Auth,
    Cache,
    Update,
}

/// Short transport-error kind. Never includes the URL (it can carry a query).
pub fn error_kind(error: &reqwest::Error) -> &'static str {
    if error.is_timeout() {
        "timed out"
    } else if error.is_connect() {
        "connect error"
    } else if error.is_redirect() {
        "redirect error"
    } else if error.is_body() {
        "body error"
    } else if error.is_decode() {
        "decode error"
    } else if error.is_builder() {
        "request build error"
    } else {
        "request error"
    }
}

/// Whether an HTTP outcome deserves a warning. 404 is expected for lookups
/// (a channel without BTTV/FFZ/7TV emotes, an unpublished release) and stays
/// at debug so it cannot flood the release log.
fn http_outcome_is_warning(outcome: Result<reqwest::StatusCode, &reqwest::Error>) -> bool {
    match outcome {
        Ok(status) => !status.is_success() && status != reqwest::StatusCode::NOT_FOUND,
        Err(_) => true,
    }
}

fn describe_http_outcome(
    method: &str,
    url: &str,
    outcome: Result<reqwest::StatusCode, &reqwest::Error>,
    elapsed: Duration,
    attempt: u32,
) -> String {
    let result = match outcome {
        Ok(status) => format!("HTTP {}", status.as_u16()),
        Err(error) => error_kind(error).to_string(),
    };
    format!(
        "{method} {} -> {result} in {} ms (attempt {attempt})",
        crate::log::redact_url(url),
        elapsed.as_millis()
    )
}

/// Log one HTTP request outcome at the network boundary: method, host + path
/// (no query or userinfo), status or error kind, duration and attempt number.
/// Debug on success and 404, warn on any other status or a transport error.
pub fn log_http_outcome(
    category: LogCategory,
    method: &str,
    url: &str,
    outcome: Result<reqwest::StatusCode, &reqwest::Error>,
    elapsed: Duration,
    attempt: u32,
) {
    let warning = http_outcome_is_warning(outcome);
    if !warning && !crate::log::enabled(category_name(category)) {
        return;
    }
    let line = describe_http_outcome(method, url, outcome, elapsed, attempt);
    emit_http_line(category, warning, &line);
}

fn emit_http_line(category: LogCategory, warning: bool, line: &str) {
    match (category, warning) {
        (LogCategory::Net, true) => crate::log_net!(warn, "{line}"),
        (LogCategory::Net, false) => crate::log_net!(debug, "{line}"),
        (LogCategory::Emotes, true) => crate::log_emotes!(warn, "{line}"),
        (LogCategory::Emotes, false) => crate::log_emotes!(debug, "{line}"),
        (LogCategory::Auth, true) => crate::log_auth!(warn, "{line}"),
        (LogCategory::Auth, false) => crate::log_auth!(debug, "{line}"),
        (LogCategory::Cache, true) => crate::log_cache!(warn, "{line}"),
        (LogCategory::Cache, false) => crate::log_cache!(debug, "{line}"),
        (LogCategory::Update, true) => crate::log_update!(warn, "{line}"),
        (LogCategory::Update, false) => crate::log_update!(debug, "{line}"),
    }
}

fn category_name(category: LogCategory) -> &'static str {
    match category {
        LogCategory::Net => "net",
        LogCategory::Emotes => "emotes",
        LogCategory::Auth => "auth",
        LogCategory::Cache => "cache",
        LogCategory::Update => "update",
    }
}

/// Minimum time between two warnings for the same host + outcome from
/// [`send_logged_quiet`].
const QUIET_WARN_INTERVAL: Duration = Duration::from_secs(60);
const QUIET_WARNED_MAX: usize = 64;
/// Last warning per `host status` key written by [`send_logged_quiet`].
static QUIET_WARNED: std::sync::LazyLock<Mutex<HashMap<String, Instant>>> =
    std::sync::LazyLock::new(|| Mutex::new(HashMap::new()));

/// Whether a quiet-path failure for `key` may warn now. Records the warning;
/// the map never grows past [`QUIET_WARNED_MAX`] (the oldest key is evicted).
fn quiet_warning_due(logged: &mut HashMap<String, Instant>, key: String, now: Instant) -> bool {
    if logged
        .get(&key)
        .is_some_and(|last| now.saturating_duration_since(*last) < QUIET_WARN_INTERVAL)
    {
        return false;
    }
    if !logged.contains_key(&key) && logged.len() >= QUIET_WARNED_MAX {
        logged.retain(|_, last| now.saturating_duration_since(*last) < QUIET_WARN_INTERVAL);
        if logged.len() >= QUIET_WARNED_MAX
            && let Some(oldest) = logged
                .iter()
                .min_by_key(|(_, last)| **last)
                .map(|(key, _)| key.clone())
        {
            logged.remove(&oldest);
        }
    }
    logged.insert(key, now);
    true
}

/// Log a [`send_logged_quiet`] outcome: the full line only at debug, and for a
/// failure at most one warning per host + outcome per [`QUIET_WARN_INTERVAL`]
/// that names the host alone (the path can carry a chatter's login).
fn log_quiet_http_outcome(
    category: LogCategory,
    method: &str,
    url: &str,
    outcome: Result<reqwest::StatusCode, &reqwest::Error>,
    elapsed: Duration,
) {
    if http_outcome_is_warning(outcome) {
        let host = url::Url::parse(url)
            .ok()
            .and_then(|url| url.host_str().map(str::to_owned))
            .unwrap_or_else(|| "<unknown host>".to_string());
        let result = match outcome {
            Ok(status) => format!("HTTP {}", status.as_u16()),
            Err(error) => error_kind(error).to_string(),
        };
        let key = format!("{host} {result}");
        if quiet_warning_due(&mut QUIET_WARNED.lock_or_recover(), key, Instant::now()) {
            emit_http_line(
                category,
                true,
                &format!(
                    "{method} {host} -> {result}; repeats within {} s log at debug",
                    QUIET_WARN_INTERVAL.as_secs()
                ),
            );
        }
    }
    if crate::log::enabled(category_name(category)) {
        emit_http_line(
            category,
            false,
            &describe_http_outcome(method, url, outcome, elapsed, 1),
        );
    }
}

/// Send `request` and log its outcome with [`log_http_outcome`] (attempt 1).
pub async fn send_logged(
    category: LogCategory,
    request: reqwest::RequestBuilder,
) -> reqwest::Result<reqwest::Response> {
    send_logged_with(category, request, false).await
}

/// [`send_logged`] for high-frequency lookups (per chatter, per visible row)
/// whose URLs can name users: see [`log_quiet_http_outcome`].
pub async fn send_logged_quiet(
    category: LogCategory,
    request: reqwest::RequestBuilder,
) -> reqwest::Result<reqwest::Response> {
    send_logged_with(category, request, true).await
}

async fn send_logged_with(
    category: LogCategory,
    request: reqwest::RequestBuilder,
    quiet: bool,
) -> reqwest::Result<reqwest::Response> {
    let (client, request) = request.build_split();
    let request = match request {
        Ok(request) => request,
        Err(error) => {
            log_http_outcome(category, "?", "", Err(&error), Duration::ZERO, 1);
            return Err(error);
        }
    };
    let method = request.method().clone();
    let url = request.url().to_string();
    let started = std::time::Instant::now();
    let result = client.execute(request).await;
    let outcome = result.as_ref().map(reqwest::Response::status);
    if quiet {
        log_quiet_http_outcome(category, method.as_str(), &url, outcome, started.elapsed());
    } else {
        log_http_outcome(
            category,
            method.as_str(),
            &url,
            outcome,
            started.elapsed(),
            1,
        );
    }
    result
}

/// Longest body kept by [`bounded_body`].
const ERROR_BODY_LIMIT: usize = 4 * 1024;

/// Read an error response body for a message; see [`bounded_body`].
pub async fn error_body(response: reqwest::Response) -> String {
    bounded_body(response).await
}

/// Read a small response body, capped at [`ERROR_BODY_LIMIT`] bytes. A body
/// that cannot be read becomes `<unreadable body: kind>` rather than an empty
/// string that hides the failure.
pub async fn bounded_body(mut response: reqwest::Response) -> String {
    let mut bytes = Vec::new();
    loop {
        match response.chunk().await {
            Ok(Some(chunk)) => {
                let room = ERROR_BODY_LIMIT.saturating_sub(bytes.len());
                bytes.extend_from_slice(&chunk[..chunk.len().min(room)]);
                if bytes.len() >= ERROR_BODY_LIMIT {
                    break;
                }
            }
            Ok(None) => break,
            Err(error) if bytes.is_empty() => {
                return format!("<unreadable body: {}>", error_kind(&error));
            }
            Err(_) => break,
        }
    }
    String::from_utf8_lossy(&bytes).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test;

    #[test]
    fn http_outcome_levels() {
        use reqwest::StatusCode;
        assert!(!http_outcome_is_warning(Ok(StatusCode::OK)));
        assert!(!http_outcome_is_warning(Ok(StatusCode::NO_CONTENT)));
        assert!(!http_outcome_is_warning(Ok(StatusCode::NOT_FOUND)));
        assert!(http_outcome_is_warning(Ok(StatusCode::TOO_MANY_REQUESTS)));
        assert!(http_outcome_is_warning(Ok(StatusCode::UNAUTHORIZED)));
        assert!(http_outcome_is_warning(Ok(StatusCode::BAD_GATEWAY)));
    }

    #[test]
    fn quiet_warning_once_per_key_per_interval_with_hard_cap() {
        let mut logged = HashMap::new();
        let start = Instant::now();
        assert!(quiet_warning_due(&mut logged, "h 500".into(), start));
        assert!(!quiet_warning_due(&mut logged, "h 500".into(), start));
        assert!(quiet_warning_due(&mut logged, "h 429".into(), start));
        assert!(quiet_warning_due(
            &mut logged,
            "h 500".into(),
            start + QUIET_WARN_INTERVAL
        ));
        let mut full = HashMap::new();
        for index in 0..QUIET_WARNED_MAX + 8 {
            let now = start + Duration::from_millis(index as u64);
            assert!(quiet_warning_due(&mut full, format!("h{index}"), now));
            assert!(full.len() <= QUIET_WARNED_MAX);
        }
        assert!(!full.contains_key("h0"));
    }

    #[test]
    fn http_outcome_line_has_no_query_or_userinfo() {
        let line = describe_http_outcome(
            "GET",
            "https://user:pw@api.twitch.tv/helix/users?login=a&token=secret",
            Ok(reqwest::StatusCode::TOO_MANY_REQUESTS),
            Duration::from_millis(42),
            2,
        );
        assert_eq!(
            line,
            "GET https://api.twitch.tv/helix/users -> HTTP 429 in 42 ms (attempt 2)"
        );
    }

    #[test]
    fn proxy_url_validation() {
        assert!(proxy_url_error("http://127.0.0.1:8080").is_none());
        assert!(proxy_url_error("socks5h://user:pw@proxy.example:1080").is_none());
        assert!(proxy_url_error("not a url::").is_some());
        assert!(proxy_url_error("ftp://proxy.example").is_some());
        assert!(proxy_url_error("127.0.0.1:8080").is_some());
    }

    #[test]
    fn blocked_proxy_url_is_valid() {
        assert!(reqwest::Proxy::all(BLOCKED_PROXY_URL).is_ok());
        assert!(url::Url::parse(BLOCKED_PROXY_URL).is_ok());
    }
}
