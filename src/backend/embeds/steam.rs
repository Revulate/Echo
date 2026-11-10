//! Store metadata provider for Steam-family application links.
//!
//! The public Steam Store `appdetails` endpoint supplies compact descriptive
//! metadata. This module owns no result cache; shared embed resource-flight and
//! cache machinery performs request deduplication and metadata retention.

use super::{image_media, non_empty, read_bounded_json};
use crate::backend::http;
use crate::backend::link_types::{EmbedLoadState, EmbedMetadata, LinkInfo};
pub use echo_link_core::parse_steam_app_url;
use echo_link_core::valid_steam_app_id as valid_app_id;
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::LazyLock;
use std::time::Duration;
use url::Url;

const FETCH_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_RESPONSE_BYTES: usize = 256 * 1024;
const APP_DETAILS_ENDPOINT: &str = "https://store.steampowered.com/api/appdetails";

static STEAM_HTTP_CLIENT: LazyLock<http::ClientCache> =
    LazyLock::new(|| http::ClientCache::new(build_steam_http_client));

fn build_steam_http_client() -> reqwest::Client {
    super::provider_http_client(FETCH_TIMEOUT, "Steam")
}

/// Stateless Steam Store metadata provider. `ClientCache` acquires a client at
/// request time so Echo proxy setting changes rebuild the routed client lazily.
#[derive(Debug, Default, Clone, Copy)]
pub struct SteamEmbedProvider;

impl SteamEmbedProvider {
    pub const fn new() -> Self {
        Self
    }

    pub async fn fetch(
        &self,
        link_info: &LinkInfo,
        app_id: &str,
        provider_label: &str,
    ) -> EmbedMetadata {
        let Some(app_id) = valid_app_id(app_id) else {
            return basic_metadata(link_info, None, provider_label);
        };
        let provider_label = normalized_provider_label(provider_label);
        let endpoint = format!("{APP_DETAILS_ENDPOINT}?appids={app_id}&l=english&cc=us");
        let response = match STEAM_HTTP_CLIENT.get(endpoint).send().await {
            Ok(response) if response.status().is_success() => response,
            Ok(response) => {
                tracing::debug!(status = %response.status(), "Steam appdetails request failed");
                return basic_metadata(link_info, Some(app_id), provider_label);
            }
            Err(error) => {
                tracing::debug!("Steam appdetails request failed: {error}");
                return basic_metadata(link_info, Some(app_id), provider_label);
            }
        };

        let Ok(payload) = read_bounded_json::<SteamAppDetailsResponse>(
            response,
            MAX_RESPONSE_BYTES,
            "Steam appdetails",
        )
        .await
        else {
            return basic_metadata(link_info, Some(app_id), provider_label);
        };
        let Some(data) = payload
            .get(app_id)
            .filter(|entry| entry.success)
            .and_then(|entry| entry.data.as_ref())
        else {
            return basic_metadata(link_info, Some(app_id), provider_label);
        };

        let header_url = data
            .header_image
            .as_deref()
            .and_then(normalize_steam_image_url)
            .or_else(|| Some(deterministic_header_url(app_id)));
        EmbedMetadata {
            link_info: link_info.clone(),
            thumbnail_url: header_url.clone(),
            title: non_empty(data.name.clone()),
            description: data
                .short_description
                .as_deref()
                .and_then(sanitize_short_description),
            subtitle: Some(format!("{provider_label} game")),
            duration: None,
            content_type: None,
            content_length: None,
            media: header_url.map(image_media),
            state: EmbedLoadState::Loaded,
        }
    }
}

/// Convenience entry point for central Steam/SteamDB/ProtonDB dispatch.
pub async fn fetch_steam_app_metadata(
    link_info: &LinkInfo,
    app_id: &str,
    provider_label: &str,
) -> EmbedMetadata {
    SteamEmbedProvider::new()
        .fetch(link_info, app_id, provider_label)
        .await
}

/// A successful basic card, retaining deterministic Steam header art when an
/// app ID is available even if the public metadata endpoint is unavailable.
pub fn basic_metadata(
    link_info: &LinkInfo,
    app_id: Option<&str>,
    provider_label: &str,
) -> EmbedMetadata {
    let provider_label = normalized_provider_label(provider_label);
    let header_url = app_id.and_then(valid_app_id).map(deterministic_header_url);
    EmbedMetadata {
        link_info: link_info.clone(),
        thumbnail_url: header_url.clone(),
        title: None,
        description: None,
        subtitle: Some(format!("{provider_label} game")),
        duration: None,
        content_type: None,
        content_length: None,
        media: header_url.map(image_media),
        state: EmbedLoadState::Loaded,
    }
}

fn normalized_provider_label(value: &str) -> &'static str {
    match value.trim() {
        "SteamDB" => "SteamDB",
        "ProtonDB" => "ProtonDB",
        _ => "Steam",
    }
}

fn deterministic_header_url(app_id: &str) -> String {
    format!("https://cdn.akamai.steamstatic.com/steam/apps/{app_id}/header.jpg")
}

fn normalize_steam_image_url(value: &str) -> Option<String> {
    let url = Url::parse(value.trim()).ok()?;
    (url.scheme() == "https"
        && url.username().is_empty()
        && url.password().is_none()
        && url.port().is_none()
        && url.host_str().is_some_and(|host| {
            host == "steamstatic.com"
                || host.ends_with(".steamstatic.com")
                || host == "steamusercontent.com"
                || host.ends_with(".steamusercontent.com")
                || host.ends_with(".akamaihd.net")
        }))
    .then(|| url.into())
}

fn sanitize_short_description(value: &str) -> Option<String> {
    let mut text = String::with_capacity(value.len().min(512));
    let mut inside_tag = false;
    for character in value.chars() {
        match character {
            '<' => inside_tag = true,
            '>' => inside_tag = false,
            _ if !inside_tag => text.push(character),
            _ => {}
        }
    }
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if text.is_empty() {
        return None;
    }
    let mut chars = text.chars();
    let truncated: String = chars.by_ref().take(320).collect();
    Some(if chars.next().is_some() {
        format!("{truncated}…")
    } else {
        truncated
    })
}

type SteamAppDetailsResponse = HashMap<String, SteamAppDetailsEntry>;

#[derive(Deserialize)]
struct SteamAppDetailsEntry {
    #[serde(default)]
    success: bool,
    #[serde(default)]
    data: Option<SteamAppDetails>,
}

#[derive(Deserialize)]
struct SteamAppDetails {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    header_image: Option<String>,
    #[serde(default)]
    short_description: Option<String>,
}
