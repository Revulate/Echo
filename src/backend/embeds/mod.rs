//! Embed metadata service for URL previews
//!
//! Fetches and caches metadata for YouTube, Twitch clips, Twitter/X, Reddit, and images
//! to enable rich inline previews in the chat.

pub mod cache;
pub mod fetchers;
mod html_entities;
mod html_head;
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

/// Read at most [`html_head::MAX_SCAN_BYTES`] of an HTML response: all the
/// head scanner looks at. A read error ends the body where it stopped.
async fn read_html_prefix(mut response: reqwest::Response) -> Vec<u8> {
    let max_bytes = html_head::MAX_SCAN_BYTES;
    let mut body: Vec<u8> = Vec::with_capacity(64 * 1024);
    while body.len() < max_bytes {
        match response.chunk().await {
            Ok(Some(chunk)) => {
                let take = chunk.len().min(max_bytes - body.len());
                body.extend_from_slice(&chunk[..take]);
            }
            Ok(None) | Err(_) => break,
        }
    }
    body
}

/// Remove what could reorder or hide the chat text around a preview from its
/// fetched display text: C0 and C1 control characters (tab, line feed and
/// carriage return become spaces) and the bidirectional embedding, override
/// and isolate controls (U+202A..=U+202E, U+2066..=U+2069). Directional marks
/// (LRM, RLM, ALM) and joiners stay: titles in right-to-left scripts and emoji
/// sequences use them.
fn sanitize_display_text(text: &mut String) {
    let unsafe_char = |ch: char| {
        ch.is_control() || matches!(ch, '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}')
    };
    if !text.contains(unsafe_char) {
        return;
    }
    *text = text
        .chars()
        .filter_map(|ch| match ch {
            '\t' | '\n' | '\r' => Some(' '),
            ch if unsafe_char(ch) => None,
            ch => Some(ch),
        })
        .collect();
}

/// Sanitize every display string of fetched metadata before it reaches the
/// UI (see [`sanitize_display_text`]).
fn sanitize_embed_metadata(metadata: &mut crate::backend::link_types::EmbedMetadata) {
    for text in [
        &mut metadata.title,
        &mut metadata.description,
        &mut metadata.subtitle,
        &mut metadata.duration,
    ]
    .into_iter()
    .flatten()
    {
        sanitize_display_text(text);
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test;

    #[test]
    fn preview_text_loses_controls_and_bidi_overrides() {
        let mut text = "a\u{202E}b\u{1}c\u{7F}d\u{2066}e\nf\rg\u{85}h\u{9F}i\u{2069}".to_string();
        sanitize_display_text(&mut text);
        assert_eq!(text, "abcde f ghi");
        // Marks and joiners used by real titles stay.
        let mut kept = "\u{5E9}\u{200F}\u{5DC} \u{1F468}\u{200D}\u{1F469}".to_string();
        let before = kept.clone();
        sanitize_display_text(&mut kept);
        assert_eq!(kept, before);
    }
}
