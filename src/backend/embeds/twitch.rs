//! Twitch-specific metadata retrieval for embed previews.
//!
//! This module deliberately returns provider-local data rather than routing or
//! classifying links. The central embed router owns `LinkInfo`, caching, and
//! settings policy; it can convert [`TwitchEmbedResult`] into `EmbedMetadata`.
//! Every request retains the original Twitch URL as the browser-open target.

use crate::backend::link_types::EmbedLoadState;
use crate::backend::twitch_api;
use crate::utils::truncate_text;
use scraper::{Html, Selector};

const TITLE_MAX_LEN: usize = 100;
const DESCRIPTION_MAX_LEN: usize = 200;
const SUBTITLE_MAX_LEN: usize = 100;

/// Provider-local Twitch metadata ready for central conversion to
/// `crate::backend::link_types::EmbedMetadata`.
///
/// This type intentionally has no URL field. The router must construct the
/// final `EmbedMetadata` with the original `LinkInfo`, preserving it as the
/// only browser-open target and preventing a preview asset from becoming a
/// navigation target.
#[derive(Clone, Debug, PartialEq)]
pub struct TwitchEmbedResult {
    pub thumbnail_url: Option<String>,
    pub title: Option<String>,
    pub description: Option<String>,
    pub subtitle: Option<String>,
    pub duration: Option<String>,
    pub state: EmbedLoadState,
}

impl TwitchEmbedResult {
    /// Convert this provider result after the central router has supplied the
    /// original classified URL.
    pub fn into_metadata(
        self,
        link_info: crate::backend::link_types::LinkInfo,
    ) -> crate::backend::link_types::EmbedMetadata {
        crate::backend::link_types::EmbedMetadata {
            link_info,
            thumbnail_url: self.thumbnail_url,
            title: self.title,
            description: self.description,
            subtitle: self.subtitle,
            duration: self.duration,
            content_type: None,
            content_length: None,
            media: None,
            state: self.state,
        }
    }
}

/// Metadata for a Twitch clip slug: the predictable clip-poster card.
pub fn fetch_clip(clip_slug: &str) -> TwitchEmbedResult {
    let clip_slug = clip_slug.trim();
    if clip_slug.is_empty() {
        return failed();
    }

    TwitchEmbedResult {
        thumbnail_url: Some(format!(
            "https://clips-media-assets2.twitch.tv/{clip_slug}-preview-480x272.jpg"
        )),
        title: None,
        description: None,
        subtitle: Some("Twitch Clip".to_string()),
        duration: None,
        state: EmbedLoadState::Loaded,
    }
}

/// Fetch metadata for a numeric Twitch VOD ID.
///
/// Helix is used when Echo has an active authenticated Twitch account. Twitch
/// does not expose a documented Echo proxy endpoint for VODs, so anonymous
/// callers degrade through Open Graph and finally a basic
/// static provider card.
pub async fn fetch_vod(original_url: &str, vod_id: &str) -> TwitchEmbedResult {
    let vod_id = vod_id.trim();
    if vod_id.is_empty() || !vod_id.bytes().all(|byte| byte.is_ascii_digit()) {
        return failed();
    }

    if let Ok(Some(video)) = twitch_api::get_video_metadata(vod_id).await {
        let author = non_empty(video.author_name).or_else(|| non_empty(video.author_login));
        return TwitchEmbedResult {
            thumbnail_url: video.thumbnail_url.and_then(non_empty),
            title: bounded_opt(Some(video.title), TITLE_MAX_LEN),
            description: bounded_opt(video.description, DESCRIPTION_MAX_LEN),
            subtitle: author.and_then(|value| bounded_opt(Some(value), SUBTITLE_MAX_LEN)),
            duration: video.duration.as_deref().and_then(format_helix_duration),
            state: EmbedLoadState::Loaded,
        };
    }

    fetch_public_fallback(original_url, "Twitch VOD").await
}

/// Fetch metadata for a Twitch channel login.
///
/// The existing proxy-aware profile and stream helpers supply the avatar plus
/// live title/game/poster when available. A normal offline stream response can
/// still obtain configured title/game through the authenticated Helix fallback
/// already implemented in `twitch_api`; anonymous offline channels retain the
/// profile/avatar result or degrade through public page metadata.
pub async fn fetch_channel(original_url: &str, channel_login: &str) -> TwitchEmbedResult {
    let channel_login = channel_login.trim().trim_start_matches('@');
    if channel_login.is_empty() {
        return failed();
    }

    let (profile, stream) = tokio::join!(
        twitch_api::fetch_user_profile(channel_login),
        twitch_api::get_stream_info(channel_login),
    );
    let profile = profile.ok();
    let stream = stream.ok().flatten();

    if profile.is_some() || stream.is_some() {
        let display_name = profile
            .as_ref()
            .and_then(|profile| non_empty(profile.display_name.clone()))
            .or_else(|| non_empty(channel_login.to_string()));
        let title = stream
            .as_ref()
            .and_then(|stream| non_empty(stream.title.clone()))
            .or_else(|| {
                display_name
                    .as_ref()
                    .map(|name| format!("{name} on Twitch"))
            });
        let game = stream
            .as_ref()
            .and_then(|stream| stream.game_name.clone())
            .and_then(non_empty);
        let subtitle = match (
            stream.as_ref().map(|stream| stream.is_live),
            game,
            display_name,
        ) {
            (Some(true), Some(game), Some(name)) => Some(format!("LIVE · {game} · {name}")),
            (Some(true), None, Some(name)) => Some(format!("LIVE · {name}")),
            (Some(false), Some(game), Some(name)) => Some(format!("Offline · {game} · {name}")),
            (Some(false), None, Some(name)) => Some(format!("Offline · {name}")),
            (_, Some(game), None) => Some(game),
            _ => Some("Twitch".to_string()),
        };
        let poster = stream
            .as_ref()
            .and_then(|stream| stream.thumbnail_url.as_deref())
            .and_then(render_stream_thumbnail)
            .or_else(|| {
                profile
                    .as_ref()
                    .and_then(|profile| profile.profile_image_url.clone())
            });
        let description = profile
            .as_ref()
            .and_then(|profile| profile.description.clone())
            .and_then(non_empty);

        return TwitchEmbedResult {
            thumbnail_url: poster.and_then(non_empty),
            title: title.and_then(|value| bounded_opt(Some(value), TITLE_MAX_LEN)),
            description: bounded_opt(description, DESCRIPTION_MAX_LEN),
            subtitle: subtitle.and_then(|value| bounded_opt(Some(value), SUBTITLE_MAX_LEN)),
            duration: None,
            state: EmbedLoadState::Loaded,
        };
    }

    fetch_public_fallback(original_url, "Twitch").await
}

async fn fetch_public_fallback(original_url: &str, basic_subtitle: &str) -> TwitchEmbedResult {
    if let Some(open_graph) = fetch_open_graph(original_url).await {
        return open_graph;
    }
    basic(basic_subtitle)
}

async fn fetch_open_graph(original_url: &str) -> Option<TwitchEmbedResult> {
    let response = twitch_api::TWITCH_HTTP_CLIENT
        .get(original_url)
        .send()
        .await
        .ok()?;
    if !response.status().is_success() {
        return None;
    }
    let html = response.text().await.ok()?;
    let document = Html::parse_document(&html);
    let content = |selector: &str| {
        Selector::parse(selector)
            .ok()
            .and_then(|selector| document.select(&selector).next())
            .and_then(|element| element.value().attr("content"))
            .map(str::to_string)
    };
    let title =
        content("meta[property='og:title']").or_else(|| content("meta[name='twitter:title']"));
    let thumbnail_url =
        content("meta[property='og:image']").or_else(|| content("meta[name='twitter:image']"));
    let description =
        content("meta[property='og:description']").or_else(|| content("meta[name='description']"));
    let subtitle = content("meta[property='og:site_name']").or_else(|| Some("Twitch".to_string()));

    (title.is_some() || thumbnail_url.is_some() || description.is_some()).then(|| {
        TwitchEmbedResult {
            thumbnail_url: thumbnail_url.and_then(non_empty),
            title: bounded_opt(title, TITLE_MAX_LEN),
            description: bounded_opt(description, DESCRIPTION_MAX_LEN),
            subtitle: bounded_opt(subtitle, SUBTITLE_MAX_LEN),
            duration: None,
            state: EmbedLoadState::Loaded,
        }
    })
}

fn render_stream_thumbnail(template: &str) -> Option<String> {
    let template = template.trim();
    (!template.is_empty()).then(|| {
        template
            .replace("{width}", "640")
            .replace("{height}", "360")
    })
}

fn format_helix_duration(value: &str) -> Option<String> {
    let mut total_seconds = 0_u64;
    let mut number = String::new();
    for character in value.trim().chars() {
        if character.is_ascii_digit() {
            number.push(character);
            continue;
        }
        let amount = number.parse::<u64>().ok()?;
        number.clear();
        total_seconds = total_seconds.checked_add(match character {
            'h' => amount.checked_mul(60 * 60)?,
            'm' => amount.checked_mul(60)?,
            's' => amount,
            _ => return None,
        })?;
    }
    if !number.is_empty() || total_seconds == 0 {
        return None;
    }
    let hours = total_seconds / 3600;
    let minutes = (total_seconds % 3600) / 60;
    let seconds = total_seconds % 60;
    Some(if hours > 0 {
        format!("{hours}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes}:{seconds:02}")
    })
}

fn bounded_opt(value: Option<String>, max_len: usize) -> Option<String> {
    value
        .and_then(non_empty)
        .map(|value| truncate_text(&value, max_len))
}

fn non_empty(value: String) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_string())
}

fn basic(subtitle: &str) -> TwitchEmbedResult {
    TwitchEmbedResult {
        thumbnail_url: None,
        title: None,
        description: None,
        subtitle: Some(subtitle.to_string()),
        duration: None,
        state: EmbedLoadState::Loaded,
    }
}

fn failed() -> TwitchEmbedResult {
    TwitchEmbedResult {
        thumbnail_url: None,
        title: None,
        description: None,
        subtitle: None,
        duration: None,
        state: EmbedLoadState::Failed,
    }
}
