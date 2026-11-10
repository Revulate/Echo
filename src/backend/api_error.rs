//! Typed failures of Twitch Helix and Echo API-proxy requests.
//!
//! Code that reacts to a failure (falls back to another source, renews the
//! session, retries, shows a status) matches on [`ApiErrorKind`]. The message
//! is only for logs and UI text, so rewording it never changes behaviour.

use std::fmt;

/// Why a Twitch or API-proxy request failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApiErrorKind {
    /// The request needs a signed-in account and there is none.
    NoAuth,
    /// The lookup needs the Echo API proxy and this build has none.
    ProxyNotConfigured,
    /// The signed-in account changed while the request was in flight, so its
    /// result was discarded.
    AccountChanged,
    /// Twitch or the proxy answered with this non-success HTTP status.
    Status(u16),
    /// The request succeeded but named no such user, or the data is only
    /// viewable by another account.
    NotFound,
    /// The request was not sent or its response not received. `retryable` is
    /// set for timeouts and connection failures.
    Transport { retryable: bool },
    /// Invalid input, or a response that could not be decoded.
    Other,
}

/// A failed Twitch or API-proxy request: what went wrong and a message for
/// logs and UI.
#[derive(Debug, Clone)]
pub struct ApiError {
    kind: ApiErrorKind,
    message: String,
}

impl ApiError {
    pub fn new(kind: ApiErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    pub fn no_auth(message: impl Into<String>) -> Self {
        Self::new(ApiErrorKind::NoAuth, message)
    }

    pub fn proxy_not_configured() -> Self {
        Self::new(
            ApiErrorKind::ProxyNotConfigured,
            super::constants::API_PROXY_NOT_CONFIGURED,
        )
    }

    pub fn account_changed(message: impl Into<String>) -> Self {
        Self::new(ApiErrorKind::AccountChanged, message)
    }

    pub fn status(status: reqwest::StatusCode, message: impl Into<String>) -> Self {
        Self::new(ApiErrorKind::Status(status.as_u16()), message)
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(ApiErrorKind::NotFound, message)
    }

    /// A request that failed before a response arrived, described as
    /// `"{context}: {error}"` with the request URL left out.
    pub fn transport(error: reqwest::Error, context: &str) -> Self {
        let retryable = error.is_timeout() || error.is_connect();
        Self::new(
            ApiErrorKind::Transport { retryable },
            format!("{context}: {}", error.without_url()),
        )
    }

    pub fn other(message: impl Into<String>) -> Self {
        Self::new(ApiErrorKind::Other, message)
    }

    pub fn kind(&self) -> ApiErrorKind {
        self.kind
    }

    pub fn message(&self) -> &str {
        &self.message
    }

    pub fn status_code(&self) -> Option<u16> {
        match self.kind {
            ApiErrorKind::Status(code) => Some(code),
            _ => None,
        }
    }

    pub fn is_proxy_not_configured(&self) -> bool {
        self.kind == ApiErrorKind::ProxyNotConfigured
    }

    /// Helix rejected the token (401) or its scopes (403).
    pub fn is_auth_rejection(&self) -> bool {
        matches!(self.status_code(), Some(401 | 403))
    }
}

impl fmt::Display for ApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ApiError {}

/// Lets `?` hand an [`ApiError`] to code that still reports failures as text.
impl From<ApiError> for String {
    fn from(error: ApiError) -> Self {
        error.message
    }
}
