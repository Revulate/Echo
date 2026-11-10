//! 7TV metadata provider for `7tv.app` embed cards.
//!
//! The browser URL stays in [`LinkInfo`]. This module only obtains compact,
//! descriptive metadata from the public v3 API and never owns result caching:
//! embed resource-flight/cache coordination remains the caller's responsibility.

use super::{image_media, non_empty, read_bounded_json};
use crate::backend::http;
use crate::backend::link_types::{EmbedLoadState, EmbedMetadata, LinkInfo};
use serde::Deserialize;
use std::sync::LazyLock;
use std::time::Duration;
use url::Url;

const FETCH_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_RESPONSE_BYTES: usize = 128 * 1024;
const API_BASE: &str = "https://7tv.io/v3";
const CDN_BASE: &str = "https://cdn.7tv.app/";

static SEVENTV_HTTP_CLIENT: LazyLock<http::ClientCache> =
    LazyLock::new(|| http::ClientCache::new(build_seventv_http_client));

fn build_seventv_http_client() -> reqwest::Client {
    super::provider_http_client(FETCH_TIMEOUT, "7TV")
}

/// Stateless 7TV API provider. `ClientCache` reacquires a proxy-aware client
/// for each request, so proxy changes do not leave this provider pinned to an
/// obsolete routing configuration.
#[derive(Debug, Default, Clone, Copy)]
pub struct SevenTvEmbedProvider;

impl SevenTvEmbedProvider {
    pub const fn new() -> Self {
        Self
    }

    /// Fetch metadata for a `https://7tv.app/emotes/{id}` page.
    pub async fn fetch_emote(&self, link_info: &LinkInfo, emote_id: &str) -> EmbedMetadata {
        let Some(id) = valid_api_id(emote_id) else {
            return basic_metadata(link_info);
        };

        let endpoint = format!("{API_BASE}/emotes/{id}");
        let response = match SEVENTV_HTTP_CLIENT.get(endpoint).send().await {
            Ok(response) if response.status().is_success() => response,
            Ok(response) => {
                tracing::debug!(status = %response.status(), "7TV emote metadata request failed");
                return basic_metadata(link_info);
            }
            Err(error) => {
                tracing::debug!("7TV emote metadata request failed: {error}");
                return basic_metadata(link_info);
            }
        };

        let Ok(emote) =
            read_bounded_json::<SevenTvEmoteResponse>(response, MAX_RESPONSE_BYTES, "7TV embed")
                .await
        else {
            return basic_metadata(link_info);
        };

        let name = non_empty(emote.name);
        let owner = emote.owner.and_then(owner_name);
        let thumbnail_url = emote_thumbnail(&emote.host, id);
        let subtitle = owner
            .map(|owner| format!("7TV emote by {owner}"))
            .or_else(|| Some("7TV emote".to_string()));

        EmbedMetadata {
            link_info: link_info.clone(),
            thumbnail_url: thumbnail_url.clone(),
            title: name,
            description: None,
            subtitle,
            duration: None,
            content_type: None,
            content_length: None,
            media: thumbnail_url.map(image_media),
            state: EmbedLoadState::Loaded,
        }
    }

    /// Fetch metadata for a `https://7tv.app/users/{id}` page.
    pub async fn fetch_user(&self, link_info: &LinkInfo, user_id: &str) -> EmbedMetadata {
        let Some(id) = valid_api_id(user_id) else {
            return basic_metadata(link_info);
        };

        let endpoint = format!("{API_BASE}/users/{id}");
        let response = match SEVENTV_HTTP_CLIENT.get(endpoint).send().await {
            Ok(response) if response.status().is_success() => response,
            Ok(response) => {
                tracing::debug!(status = %response.status(), "7TV user metadata request failed");
                return basic_metadata(link_info);
            }
            Err(error) => {
                tracing::debug!("7TV user metadata request failed: {error}");
                return basic_metadata(link_info);
            }
        };

        let Ok(user) =
            read_bounded_json::<SevenTvUserResponse>(response, MAX_RESPONSE_BYTES, "7TV embed")
                .await
        else {
            return basic_metadata(link_info);
        };

        let display_name = non_empty(user.display_name);
        let login = non_empty(user.username);
        let title = display_name.clone().or_else(|| login.clone());
        let subtitle = match (display_name, login) {
            (Some(display_name), Some(login)) if display_name != login => Some(format!("@{login}")),
            _ => Some("7TV user".to_string()),
        };
        let avatar_url = user
            .avatar_url
            .as_deref()
            .and_then(|url| normalize_seventv_url(url, CDN_BASE));

        EmbedMetadata {
            link_info: link_info.clone(),
            thumbnail_url: avatar_url.clone(),
            title,
            description: None,
            subtitle,
            duration: None,
            content_type: None,
            content_length: None,
            media: avatar_url.map(image_media),
            state: EmbedLoadState::Loaded,
        }
    }
}

/// Convenience entry point for central embed dispatch.
pub async fn fetch_seventv_emote_metadata(link_info: &LinkInfo, emote_id: &str) -> EmbedMetadata {
    SevenTvEmbedProvider::new()
        .fetch_emote(link_info, emote_id)
        .await
}

/// Convenience entry point for central embed dispatch.
pub async fn fetch_seventv_user_metadata(link_info: &LinkInfo, user_id: &str) -> EmbedMetadata {
    SevenTvEmbedProvider::new()
        .fetch_user(link_info, user_id)
        .await
}

/// A successful, no-network fallback for unavailable/invalid provider data.
pub fn basic_metadata(link_info: &LinkInfo) -> EmbedMetadata {
    EmbedMetadata {
        link_info: link_info.clone(),
        thumbnail_url: None,
        title: None,
        description: None,
        subtitle: Some("7TV".to_string()),
        duration: None,
        content_type: None,
        content_length: None,
        media: None,
        state: EmbedLoadState::Loaded,
    }
}

fn valid_api_id(value: &str) -> Option<&str> {
    let id = value.trim();
    (!id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-'))
    .then_some(id)
}

fn emote_thumbnail(host: &Option<SevenTvHost>, emote_id: &str) -> Option<String> {
    let fallback = format!("https://cdn.7tv.app/emote/{emote_id}/4x.webp");
    let Some(host) = host else {
        return Some(fallback);
    };
    let Some(base) = normalize_seventv_url(&host.url, CDN_BASE) else {
        return Some(fallback);
    };

    let file = host
        .files
        .iter()
        .find(|file| file.name.eq_ignore_ascii_case("4x.webp"))
        .or_else(|| {
            host.files.iter().find(|file| {
                file.format.eq_ignore_ascii_case("webp") && file.name.starts_with("4x.")
            })
        })
        .or_else(|| {
            host.files
                .iter()
                .find(|file| file.name.eq_ignore_ascii_case("2x.webp"))
        })
        .or_else(|| {
            host.files
                .iter()
                .find(|file| file.name.eq_ignore_ascii_case("1x.webp"))
        })
        .or_else(|| {
            host.files
                .iter()
                .find(|file| file.format.eq_ignore_ascii_case("webp"))
        });

    file.and_then(|file| join_cdn_path(&base, &file.name))
        .or(Some(fallback))
}

fn owner_name(owner: SevenTvOwner) -> Option<String> {
    non_empty(owner.display_name).or_else(|| non_empty(owner.username))
}

/// Convert 7TV's scheme-relative and relative CDN values into an HTTPS URL.
/// Only 7TV-owned HTTPS origins are admitted, preventing API response data
/// from turning this metadata route into an arbitrary remote-media fetch.
fn normalize_seventv_url(value: &str, relative_base: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty() || value.starts_with('#') || value.starts_with('?') {
        return None;
    }

    let parsed = if let Some(value) = value.strip_prefix("//") {
        Url::parse(&format!("https://{value}")).ok()?
    } else if let Ok(url) = Url::parse(value) {
        url
    } else if value.starts_with("cdn.7tv.app/") {
        Url::parse(&format!("https://{value}")).ok()?
    } else {
        Url::parse(relative_base).ok()?.join(value).ok()?
    };

    (parsed.scheme() == "https"
        && parsed.username().is_empty()
        && parsed.password().is_none()
        && parsed.port().is_none()
        && parsed
            .host_str()
            .is_some_and(|host| host == "7tv.app" || host.ends_with(".7tv.app")))
    .then(|| parsed.into())
}

fn join_cdn_path(base: &str, file: &str) -> Option<String> {
    let file = file.trim().trim_start_matches('/');
    if file.is_empty() || file.split('/').any(|part| part == "..") {
        return None;
    }
    Url::parse(base).ok()?.join(file).ok().and_then(|url| {
        let url = url.to_string();
        normalize_seventv_url(&url, CDN_BASE)
    })
}

#[derive(Deserialize)]
struct SevenTvEmoteResponse {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    owner: Option<SevenTvOwner>,
    #[serde(default)]
    host: Option<SevenTvHost>,
}

#[derive(Deserialize)]
struct SevenTvOwner {
    #[serde(default)]
    display_name: Option<String>,
    #[serde(default)]
    username: Option<String>,
}

#[derive(Deserialize)]
struct SevenTvHost {
    #[serde(default)]
    url: String,
    #[serde(default)]
    files: Vec<SevenTvFile>,
}

#[derive(Deserialize)]
struct SevenTvFile {
    #[serde(default)]
    name: String,
    #[serde(default)]
    format: String,
}

#[derive(Deserialize)]
struct SevenTvUserResponse {
    #[serde(default)]
    display_name: Option<String>,
    #[serde(default)]
    username: Option<String>,
    #[serde(default)]
    avatar_url: Option<String>,
}
