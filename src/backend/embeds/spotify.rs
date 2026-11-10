//! Credential-free Spotify metadata provider for link preview cards.
//!
//! This provider intentionally consumes only Spotify's public oEmbed response.
//! It does not parse/embed the returned HTML and does not keep a result cache;
//! the shared embed resource-flight/cache layer owns those responsibilities.

use super::{image_media, non_empty, read_bounded_json};
use crate::backend::http;
use crate::backend::link_types::{EmbedLoadState, EmbedMetadata, LinkInfo};
pub use echo_link_core::{SpotifyResourceKind, parse_spotify_resource_url};
use serde::Deserialize;
use std::sync::LazyLock;
use std::time::Duration;
use url::Url;

const FETCH_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_RESPONSE_BYTES: usize = 128 * 1024;
const OEMBED_ENDPOINT: &str = "https://open.spotify.com/oembed";

static SPOTIFY_HTTP_CLIENT: LazyLock<http::ClientCache> =
    LazyLock::new(|| http::ClientCache::new(build_spotify_http_client));

fn build_spotify_http_client() -> reqwest::Client {
    super::provider_http_client(FETCH_TIMEOUT, "Spotify")
}

/// Stateless public-oEmbed Spotify provider. Its `ClientCache` obtains a new
/// proxy-aware request client whenever Echo's network routing generation changes.
#[derive(Debug, Default, Clone, Copy)]
pub struct SpotifyEmbedProvider;

impl SpotifyEmbedProvider {
    pub const fn new() -> Self {
        Self
    }

    pub async fn fetch(
        &self,
        link_info: &LinkInfo,
        resource_kind: SpotifyResourceKind,
        spotify_id: &str,
    ) -> EmbedMetadata {
        if !is_supported_resource_url(&link_info.url, resource_kind, spotify_id) {
            return basic_metadata(link_info, resource_kind);
        }

        let endpoint = format!(
            "{OEMBED_ENDPOINT}?url={}",
            urlencoding::encode(&link_info.url)
        );
        let response = match SPOTIFY_HTTP_CLIENT.get(endpoint).send().await {
            Ok(response) if response.status().is_success() => response,
            Ok(response) => {
                crate::log_net!(debug, status = %response.status(), "Spotify oEmbed request failed");
                return basic_metadata(link_info, resource_kind);
            }
            Err(error) => {
                crate::log_net!(debug, "Spotify oEmbed request failed: {error}");
                return basic_metadata(link_info, resource_kind);
            }
        };

        let Ok(metadata) = read_bounded_json::<SpotifyOEmbedResponse>(
            response,
            MAX_RESPONSE_BYTES,
            "Spotify oEmbed",
        )
        .await
        else {
            return basic_metadata(link_info, resource_kind);
        };

        let artwork_url = metadata
            .thumbnail_url
            .as_deref()
            .and_then(normalize_spotify_artwork_url);
        EmbedMetadata {
            link_info: link_info.clone(),
            thumbnail_url: artwork_url.clone(),
            title: non_empty(metadata.title),
            description: None,
            subtitle: Some(format!("Spotify {}", resource_kind.as_str())),
            duration: None,
            content_type: None,
            content_length: None,
            media: artwork_url.map(image_media),
            state: EmbedLoadState::Loaded,
        }
    }
}

/// Convenience entry point for central embed dispatch.
pub async fn fetch_spotify_metadata(
    link_info: &LinkInfo,
    resource_kind: SpotifyResourceKind,
    spotify_id: &str,
) -> EmbedMetadata {
    SpotifyEmbedProvider::new()
        .fetch(link_info, resource_kind, spotify_id)
        .await
}

/// A successful, no-network fallback for missing or unavailable oEmbed data.
pub fn basic_metadata(link_info: &LinkInfo, resource_kind: SpotifyResourceKind) -> EmbedMetadata {
    EmbedMetadata {
        link_info: link_info.clone(),
        thumbnail_url: None,
        title: None,
        description: None,
        subtitle: Some(format!("Spotify {}", resource_kind.as_str())),
        duration: None,
        content_type: None,
        content_length: None,
        media: None,
        state: EmbedLoadState::Loaded,
    }
}

fn is_supported_resource_url(
    value: &str,
    resource_kind: SpotifyResourceKind,
    spotify_id: &str,
) -> bool {
    let Some((parsed_kind, parsed_id)) = parse_spotify_resource_url(value) else {
        return false;
    };
    parsed_kind == resource_kind && parsed_id == spotify_id
}

fn normalize_spotify_artwork_url(value: &str) -> Option<String> {
    let url = Url::parse(value.trim()).ok()?;
    (url.scheme() == "https"
        && url.username().is_empty()
        && url.password().is_none()
        && url.port().is_none()
        && url.host_str().is_some_and(|host| {
            host == "i.scdn.co"
                || host.ends_with(".scdn.co")
                || host == "spotifycdn.com"
                || host.ends_with(".spotifycdn.com")
        }))
    .then(|| url.into())
}

#[derive(Deserialize)]
struct SpotifyOEmbedResponse {
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    thumbnail_url: Option<String>,
}
