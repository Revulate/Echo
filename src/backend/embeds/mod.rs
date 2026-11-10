//! Embed metadata service for URL previews
//!
//! Fetches and caches metadata for YouTube, Twitch clips, Twitter/X, Reddit, and images
//! to enable rich inline previews in the chat.

pub mod cache;
pub mod fetchers;
pub mod seventv;
pub mod spotify;
pub mod state;
pub mod steam;
pub mod twitch;

pub use fetchers::fetch_embed_metadata;

use crate::backend::http;
use crate::backend::link_types::{MediaAsset, MediaKind};
use std::time::Duration;

fn provider_http_client(timeout: Duration, label: &str) -> reqwest::Client {
    http::client_builder()
        .timeout(timeout)
        .pool_idle_timeout(Duration::from_secs(30))
        .pool_max_idle_per_host(2)
        .build()
        .unwrap_or_else(|error| {
            crate::log_net!(warn, "Failed to create {label} embed HTTP client: {error}");
            http::timed_client(timeout)
        })
}

async fn read_bounded_json<T: serde::de::DeserializeOwned>(
    mut response: reqwest::Response,
    max_bytes: usize,
    label: &str,
) -> Result<T, ()> {
    if response
        .content_length()
        .is_some_and(|length| length > max_bytes as u64)
    {
        crate::log_net!(debug, "{label} response exceeded {} KiB", max_bytes / 1024);
        return Err(());
    }

    let mut body = Vec::with_capacity(
        response
            .content_length()
            .and_then(|length| usize::try_from(length).ok())
            .unwrap_or_default()
            .min(max_bytes),
    );
    loop {
        let chunk = response.chunk().await.map_err(|error| {
            crate::log_net!(debug, "Failed reading {label} response: {error}");
        })?;
        let Some(chunk) = chunk else {
            break;
        };
        if chunk.len() > max_bytes.saturating_sub(body.len()) {
            crate::log_net!(debug, "{label} response exceeded {} KiB", max_bytes / 1024);
            return Err(());
        }
        body.extend_from_slice(&chunk);
    }

    serde_json::from_slice(&body).map_err(|error| {
        crate::log_net!(debug, "Invalid {label} response: {error}");
    })
}

fn non_empty(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn image_media(source_url: String) -> MediaAsset {
    MediaAsset {
        kind: MediaKind::Image,
        poster_url: Some(source_url.clone()),
        source_url,
    }
}
