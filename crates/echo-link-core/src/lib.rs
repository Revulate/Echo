//! Link type detection utilities for embeds.
//!
//! Detects various link types (images, YouTube, Twitch clips, Twitter, Reddit)
//! and extracts relevant metadata for embed rendering.

use regex::Regex;
use serde::{Deserialize, Serialize};
use std::sync::LazyLock;

pub mod image_uploader;
mod providers;

pub use providers::{
    SpotifyResourceKind, SteamAppSource, parse_spotify_resource_url, parse_steam_app_url,
    valid_steam_app_id,
};

/// Closed set of preview providers. Unknown hosts deliberately remain a
/// separate provider so a future resolver cannot silently inherit another
/// provider's privacy policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Provider {
    Direct,
    SevenTv,
    Spotify,
    Steam,
    Twitch,
    YouTube,
    Twitter,
    Reddit,
    Generic,
}

/// Closed set of preview content classes used by the policy settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ContentKind {
    Image,
    Audio,
    Clip,
    Video,
    Channel,
    Emote,
    Profile,
    Post,
    Generic,
}

/// Canonical identity of the remote resource, distinct from one rendered URL
/// occurrence.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ResourceKey(pub String);

impl ResourceKey {
    pub fn for_url(url: &str) -> Self {
        let canonical = url::Url::parse(url)
            .map(|mut parsed| {
                parsed.set_fragment(None);
                let _ = parsed.set_scheme(&parsed.scheme().to_ascii_lowercase());
                if let Some(host) = parsed.host_str().map(str::to_ascii_lowercase) {
                    let _ = parsed.set_host(Some(&host));
                }
                if matches!(
                    (parsed.scheme(), parsed.port()),
                    ("http", Some(80)) | ("https", Some(443))
                ) {
                    let _ = parsed.set_port(None);
                }
                parsed.to_string()
            })
            .unwrap_or_else(|_| url.to_string());
        Self(canonical)
    }
}

/// Classification is resource-scoped. Rendering and disclosure retain their
/// own occurrence identity (message id plus embed id).
#[derive(Debug, Clone, PartialEq)]
pub struct ClassifiedLink {
    pub provider: Provider,
    pub content_kind: ContentKind,
    pub resource_key: ResourceKey,
    pub link_info: LinkInfo,
}

pub fn classify_link(url: &str) -> ClassifiedLink {
    let link_info = get_link_type(url);
    let (provider, content_kind) = match link_info.link_type {
        LinkType::Image => (Provider::Direct, ContentKind::Image),
        LinkType::Video => (Provider::Direct, ContentKind::Video),
        LinkType::TwitchClip => (Provider::Twitch, ContentKind::Clip),
        LinkType::TwitchVod => (Provider::Twitch, ContentKind::Video),
        LinkType::TwitchChannel => (Provider::Twitch, ContentKind::Channel),
        LinkType::SevenTvEmote => (Provider::SevenTv, ContentKind::Emote),
        LinkType::SevenTvUser => (Provider::SevenTv, ContentKind::Profile),
        LinkType::Spotify => (Provider::Spotify, ContentKind::Audio),
        LinkType::Steam => (Provider::Steam, ContentKind::Generic),
        LinkType::YouTube => (Provider::YouTube, ContentKind::Video),
        LinkType::Twitter => (Provider::Twitter, ContentKind::Post),
        LinkType::Reddit => (Provider::Reddit, ContentKind::Post),
        LinkType::Generic => (Provider::Generic, ContentKind::Generic),
    };

    ClassifiedLink {
        provider,
        content_kind,
        resource_key: provider_resource_key(&link_info)
            .unwrap_or_else(|| ResourceKey::for_url(url)),
        link_info,
    }
}

/// Types of links that can be embedded
#[derive(Debug, Clone, PartialEq)]
pub enum LinkType {
    Image,
    /// Direct video file. Echo presents it as static metadata/poster art; the
    /// original URL remains the browser-open target and is never streamed.
    Video,
    TwitchClip,
    TwitchVod,
    TwitchChannel,
    SevenTvEmote,
    SevenTvUser,
    /// Canonical `https://open.spotify.com/{kind}/{id}` public resource.
    Spotify,
    /// Canonical Steam Store, SteamDB, or ProtonDB application page.
    Steam,
    YouTube,
    Twitter,
    Reddit,
    Generic,
}

/// Information about a detected link
#[derive(Debug, Clone, PartialEq)]
pub struct LinkInfo {
    pub link_type: LinkType,
    pub url: String,
    /// Friendly provider/site label when known
    pub provider_name: Option<String>,
    /// Twitch clip ID
    pub clip_id: Option<String>,
    /// Twitch VOD ID
    pub vod_id: Option<String>,
    /// Twitch channel login
    pub channel_login: Option<String>,
    /// 7TV emote ID
    pub seventv_emote_id: Option<String>,
    /// 7TV user ID
    pub seventv_user_id: Option<String>,
    /// YouTube video ID
    pub video_id: Option<String>,
    /// Twitter/X tweet ID
    pub tweet_id: Option<String>,
    /// Reddit subreddit
    pub subreddit: Option<String>,
    /// Reddit post ID
    pub post_id: Option<String>,
}

impl LinkInfo {
    /// A link of `link_type` with no provider-specific id; detectors set the
    /// one id their link type carries with struct-update syntax.
    pub fn new(link_type: LinkType, url: impl Into<String>, provider_name: Option<String>) -> Self {
        Self {
            link_type,
            url: url.into(),
            provider_name,
            clip_id: None,
            vod_id: None,
            channel_login: None,
            seventv_emote_id: None,
            seventv_user_id: None,
            video_id: None,
            tweet_id: None,
            subreddit: None,
            post_id: None,
        }
    }
}

/// Loading state for embed metadata
#[derive(Debug, Clone, PartialEq, Default)]
pub enum EmbedLoadState {
    #[default]
    Pending,
    Loaded,
    Failed,
}

/// Closed media kinds stored with resource-scoped preview metadata. Media is
/// descriptive only: it does not grant the renderer a playback path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaKind {
    Image,
    Video,
}

/// Static media information for a preview resource. `source_url` may point to
/// a direct media file, while the surrounding `LinkInfo::url` always remains
/// the user-confirmed browser target for the preview card.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaAsset {
    pub kind: MediaKind,
    pub source_url: String,
    pub poster_url: Option<String>,
}

/// Rich metadata for embed previews
#[derive(Debug, Clone, PartialEq)]
pub struct EmbedMetadata {
    pub link_info: LinkInfo,
    /// Thumbnail URL for preview
    pub thumbnail_url: Option<String>,
    /// Title of the content
    pub title: Option<String>,
    /// Description from Open Graph or ordinary HTML metadata, distinct from
    /// the site/provider subtitle.
    pub description: Option<String>,
    /// Subtitle (channel name, author, subreddit, etc.)
    pub subtitle: Option<String>,
    /// Duration string for videos (e.g., "3:45")
    pub duration: Option<String>,
    /// MIME type from response headers (e.g. "image/jpeg"), when known.
    pub content_type: Option<String>,
    /// Content-Length from response headers, when known.
    pub content_length: Option<u64>,
    /// Resource-scoped static media semantics. Video assets are never passed
    /// to a native player or stream decoder.
    pub media: Option<MediaAsset>,
    /// Terminal fetch state. In-progress state belongs to the bounded
    /// resource-flight registry, never to resource-scoped cached metadata.
    pub state: EmbedLoadState,
}

// Image extensions regex
static IMAGE_EXTENSIONS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\.(png|jpg|jpeg|gif|webp|bmp|svg)(\?.*)?$").unwrap());

// Direct video files are previewed only as static cards/posters. Keep this
// distinct from provider-hosted clips, which have their own metadata paths.
static VIDEO_EXTENSIONS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\.(mp4|webm|mov|m4v|mkv)(\?.*)?$").unwrap());

/// Detect the type of a link and extract relevant metadata
pub fn get_link_type(url: &str) -> LinkInfo {
    let provider_name = provider_name_for_url(url);

    // Check for direct images first
    if IMAGE_EXTENSIONS.is_match(url) {
        return LinkInfo::new(
            LinkType::Image,
            url,
            provider_name.clone().or_else(|| Some("Image".to_string())),
        );
    }

    if VIDEO_EXTENSIONS.is_match(url) {
        return LinkInfo::new(
            LinkType::Video,
            url,
            provider_name.clone().or_else(|| Some("Video".to_string())),
        );
    }

    if let Ok(parsed) = url::Url::parse(url) {
        if let Some(link_info) = detect_youtube_from_url(&parsed, url) {
            return link_info;
        }
        if let Some(link_info) = detect_twitch_from_url(&parsed, url) {
            return link_info;
        }
        if let Some(link_info) = detect_seventv_from_url(&parsed, url) {
            return link_info;
        }
        if let Some(link_info) = detect_spotify_from_url(&parsed, url) {
            return link_info;
        }
        if let Some(link_info) = detect_steam_from_url(&parsed, url) {
            return link_info;
        }
        if let Some(link_info) = detect_twitter_from_url(&parsed, url) {
            return link_info;
        }
        if let Some(link_info) = detect_reddit_from_url(&parsed, url) {
            return link_info;
        }
    }

    // Default to generic
    LinkInfo::new(LinkType::Generic, url, provider_name)
}

/// Get YouTube thumbnail URL for a video ID
pub fn get_youtube_thumbnail(video_id: &str) -> String {
    format!("https://img.youtube.com/vi/{}/mqdefault.jpg", video_id)
}

/// Extract domain from a URL
pub fn get_domain(url: &str) -> String {
    url::Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(|s| s.to_string()))
        .unwrap_or_else(|| url.to_string())
}

/// "image/jpeg" -> "JPEG", "image/png" -> "PNG", "text/html" -> "HTML", else uppercased subtype.
pub fn short_content_type_label(content_type: &str) -> Option<String> {
    let mime_type = content_type.split(';').next()?.trim();
    let (type_name, subtype) = mime_type.split_once('/')?;
    let subtype = subtype.trim();
    if type_name.trim().is_empty() || subtype.is_empty() {
        return None;
    }
    Some(subtype.to_ascii_uppercase())
}

/// 629145 -> "0.6 MB"; < 1 MB uses one decimal, >= 10 MB rounds to whole MB, < 100 KB shows KB.
pub fn format_content_length(bytes: u64) -> String {
    const KIB: f64 = 1024.0;
    const MIB: f64 = KIB * 1024.0;

    if bytes < 100 * 1024 {
        format!("{:.1} KB", bytes as f64 / KIB)
    } else {
        let megabytes = bytes as f64 / MIB;
        if megabytes >= 10.0 {
            format!("{:.0} MB", megabytes)
        } else {
            format!("{:.1} MB", megabytes)
        }
    }
}

pub fn provider_name_for_url(url: &str) -> Option<String> {
    let host = normalized_host(url::Url::parse(url).ok()?.host_str()?)?;
    Some(
        match host.as_str() {
            "youtube.com" | "youtu.be" => "YouTube",
            "7tv.app" => "7TV",
            "open.spotify.com" => "Spotify",
            "store.steampowered.com" => "Steam",
            "steamdb.info" => "SteamDB",
            "protondb.com" => "ProtonDB",
            "twitch.tv" | "clips.twitch.tv" => "Twitch",
            "twitter.com" | "x.com" => "Twitter/X",
            "reddit.com" | "redd.it" => "Reddit",
            "discord.com" => "Discord",
            "imgur.com" => "Imgur",
            "wikipedia.org" => "Wikipedia",
            "betterttv.com" => "BetterTTV",
            "frankerfacez.com" => "FrankerFaceZ",
            "livestreamfails.com" => "Livestreamfails",
            "supinic.com" => "Supinic",
            _ => return None,
        }
        .to_string(),
    )
}

/// Keep Spotify's URL classifier in its provider module, so central routing
/// and the oEmbed fetcher accept exactly the same narrow public URL surface.
fn detect_spotify_from_url(parsed: &url::Url, original_url: &str) -> Option<LinkInfo> {
    parse_spotify_resource_url(parsed.as_str())
        .map(|_| LinkInfo::new(LinkType::Spotify, original_url, Some("Spotify".to_string())))
}

/// Keep Steam-family URL classification in its provider module so central
/// routing and the public appdetails fetcher accept exactly the same path set.
fn detect_steam_from_url(parsed: &url::Url, original_url: &str) -> Option<LinkInfo> {
    parse_steam_app_url(parsed.as_str()).map(|(source, _)| {
        LinkInfo::new(
            LinkType::Steam,
            original_url,
            Some(source.provider_label().to_string()),
        )
    })
}

fn detect_youtube_from_url(parsed: &url::Url, original_url: &str) -> Option<LinkInfo> {
    let host = normalized_host(parsed.host_str()?)?;
    let video_id = match host.as_str() {
        "youtube.com" => {
            let segments = path_segments(parsed);
            if matches!(segments.first().copied(), Some("watch")) {
                parsed.query_pairs().find_map(|(key, value)| {
                    (key == "v" && is_youtube_video_id(&value)).then(|| value.into_owned())
                })
            } else if matches!(segments.first().copied(), Some("embed" | "shorts" | "live")) {
                segments
                    .get(1)
                    .filter(|id| is_youtube_video_id(id))
                    .map(|id| (*id).to_string())
            } else {
                None
            }
        }
        "youtu.be" => path_segments(parsed)
            .first()
            .filter(|id| is_youtube_video_id(id))
            .map(|id| (*id).to_string()),
        _ => None,
    }?;

    Some(LinkInfo {
        video_id: Some(video_id),
        ..LinkInfo::new(LinkType::YouTube, original_url, Some("YouTube".to_string()))
    })
}

fn detect_twitch_from_url(parsed: &url::Url, original_url: &str) -> Option<LinkInfo> {
    let host = normalized_host(parsed.host_str()?)?;
    let segments = path_segments(parsed);
    match host.as_str() {
        "clips.twitch.tv" => twitch_clip_link_info(segments.first().copied(), original_url),
        "twitch.tv" => match segments.as_slice() {
            [channel, "clip", clip_id, ..] if is_twitch_login(channel) => {
                twitch_clip_link_info(Some(clip_id), original_url)
            }
            ["videos", vod_id, ..] if is_numeric_segment(vod_id) => Some(LinkInfo {
                vod_id: Some((*vod_id).to_string()),
                ..LinkInfo::new(
                    LinkType::TwitchVod,
                    original_url,
                    Some("Twitch".to_string()),
                )
            }),
            [channel] if is_twitch_login(channel) => Some(LinkInfo {
                channel_login: Some(channel.to_ascii_lowercase()),
                ..LinkInfo::new(
                    LinkType::TwitchChannel,
                    original_url,
                    Some("Twitch".to_string()),
                )
            }),
            _ => None,
        },
        _ => None,
    }
}

fn twitch_clip_link_info(clip_id: Option<&str>, original_url: &str) -> Option<LinkInfo> {
    let clip_id = clip_id?.trim();
    (!clip_id.is_empty()).then(|| LinkInfo {
        clip_id: Some(clip_id.to_string()),
        ..LinkInfo::new(
            LinkType::TwitchClip,
            original_url,
            Some("Twitch".to_string()),
        )
    })
}

/// Recognize only canonical 7TV emote and user pages. Other 7TV routes remain
/// generic links, so they cannot inherit this provider's API-fetch policy.
fn detect_seventv_from_url(parsed: &url::Url, original_url: &str) -> Option<LinkInfo> {
    let host = parsed.host_str()?.trim_end_matches('.');
    if !host.eq_ignore_ascii_case("7tv.app") {
        return None;
    }

    let segments = path_segments(parsed);
    match segments.as_slice() {
        ["emotes", emote_id] if is_seventv_id(emote_id) => Some(LinkInfo {
            seventv_emote_id: Some((*emote_id).to_string()),
            ..LinkInfo::new(
                LinkType::SevenTvEmote,
                original_url,
                Some("7TV".to_string()),
            )
        }),
        ["users", user_id] if is_seventv_id(user_id) => Some(LinkInfo {
            seventv_user_id: Some((*user_id).to_string()),
            ..LinkInfo::new(LinkType::SevenTvUser, original_url, Some("7TV".to_string()))
        }),
        _ => None,
    }
}

fn detect_twitter_from_url(parsed: &url::Url, original_url: &str) -> Option<LinkInfo> {
    let host = normalized_host(parsed.host_str()?)?;
    if !matches!(host.as_str(), "twitter.com" | "x.com") {
        return None;
    }

    let segments = path_segments(parsed);
    let tweet_id = segments
        .windows(2)
        .find_map(|window| {
            (window[0] == "status" && is_numeric_segment(window[1])).then(|| window[1])
        })?
        .to_string();

    Some(LinkInfo {
        tweet_id: Some(tweet_id),
        ..LinkInfo::new(
            LinkType::Twitter,
            original_url,
            Some("Twitter/X".to_string()),
        )
    })
}

fn detect_reddit_from_url(parsed: &url::Url, original_url: &str) -> Option<LinkInfo> {
    let host = normalized_host(parsed.host_str()?)?;
    let segments = path_segments(parsed);

    match host.as_str() {
        "redd.it" => Some(LinkInfo {
            post_id: segments.first().map(|segment| (*segment).to_string()),
            ..LinkInfo::new(LinkType::Reddit, original_url, Some("Reddit".to_string()))
        }),
        "reddit.com" => {
            let subreddit = segments
                .windows(2)
                .find_map(|window| (window[0] == "r").then(|| window[1]))?
                .to_string();
            let post_id = segments
                .windows(2)
                .find_map(|window| {
                    matches!(window[0], "comments" | "s")
                        .then(|| window[1])
                        .filter(|segment| !segment.is_empty())
                })
                .map(str::to_string)?;
            Some(LinkInfo {
                subreddit: Some(subreddit),
                post_id: Some(post_id),
                ..LinkInfo::new(LinkType::Reddit, original_url, Some("Reddit".to_string()))
            })
        }
        _ => None,
    }
}

fn normalized_host(host: &str) -> Option<String> {
    let host = host.trim().trim_end_matches('.').to_ascii_lowercase();
    let host = host
        .strip_prefix("www.")
        .or_else(|| host.strip_prefix("m."))
        .or_else(|| host.strip_prefix("mobile."))
        .unwrap_or(&host)
        .to_string();
    if host == "youtube.com" || host.ends_with(".youtube.com") {
        Some("youtube.com".to_string())
    } else if host == "youtu.be" {
        Some("youtu.be".to_string())
    } else if host.ends_with(".twitch.tv") {
        Some(if host == "clips.twitch.tv" {
            "clips.twitch.tv".to_string()
        } else {
            "twitch.tv".to_string()
        })
    } else if host == "twitch.tv" {
        Some("twitch.tv".to_string())
    } else if host == "twitter.com" || host.ends_with(".twitter.com") {
        Some("twitter.com".to_string())
    } else if host == "x.com" || host.ends_with(".x.com") {
        Some("x.com".to_string())
    } else if host == "reddit.com" || host.ends_with(".reddit.com") {
        Some("reddit.com".to_string())
    } else if host == "redd.it" {
        Some("redd.it".to_string())
    } else if host == "wikipedia.org" || host.ends_with(".wikipedia.org") {
        Some("wikipedia.org".to_string())
    } else if host == "imgur.com" || host.ends_with(".imgur.com") {
        Some("imgur.com".to_string())
    } else if host == "discord.com" || host.ends_with(".discord.com") || host == "discord.gg" {
        Some("discord.com".to_string())
    } else if host == "betterttv.com" || host.ends_with(".betterttv.com") {
        Some("betterttv.com".to_string())
    } else if host == "frankerfacez.com" || host.ends_with(".frankerfacez.com") {
        Some("frankerfacez.com".to_string())
    } else if host == "livestreamfails.com" || host.ends_with(".livestreamfails.com") {
        Some("livestreamfails.com".to_string())
    } else if host == "supinic.com" || host.ends_with(".supinic.com") {
        Some("supinic.com".to_string())
    } else if host.is_empty() {
        None
    } else {
        Some(host)
    }
}

fn path_segments(parsed: &url::Url) -> Vec<&str> {
    parsed
        .path_segments()
        .map(|segments| {
            segments
                .filter(|segment| !segment.is_empty())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default()
}

fn is_youtube_video_id(value: &str) -> bool {
    value.len() == 11
        && value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_'))
}

fn is_numeric_segment(value: &str) -> bool {
    !value.is_empty() && value.chars().all(|ch| ch.is_ascii_digit())
}

fn is_seventv_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

/// Twitch channel logins are 4-25 ASCII letters, digits, or underscores. A
/// reserved route is not a channel even when it otherwise has that shape.
fn is_twitch_login(value: &str) -> bool {
    const RESERVED_TWITCH_ROUTES: &[&str] = &[
        "about",
        "activate",
        "apps",
        "blog",
        "creatorcamp",
        "directory",
        "downloads",
        "inventory",
        "jobs",
        "legal",
        "login",
        "p",
        "partners",
        "privacy",
        "products",
        "search",
        "security",
        "settings",
        "signup",
        "store",
        "subscriptions",
        "support",
        "team",
        "turbo",
        "videos",
        "wallet",
    ];
    let value = value.trim();
    (4..=25).contains(&value.len())
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        && !RESERVED_TWITCH_ROUTES.contains(&value.to_ascii_lowercase().as_str())
}

fn provider_resource_key(link_info: &LinkInfo) -> Option<ResourceKey> {
    let (provider, kind, id) = match &link_info.link_type {
        LinkType::TwitchClip => ("twitch", "clip", link_info.clip_id.as_deref()?),
        LinkType::TwitchVod => ("twitch", "vod", link_info.vod_id.as_deref()?),
        LinkType::TwitchChannel => ("twitch", "channel", link_info.channel_login.as_deref()?),
        LinkType::SevenTvEmote => ("seventv", "emote", link_info.seventv_emote_id.as_deref()?),
        LinkType::SevenTvUser => ("seventv", "user", link_info.seventv_user_id.as_deref()?),
        LinkType::Spotify => {
            let (kind, id) = parse_spotify_resource_url(&link_info.url)?;
            return Some(ResourceKey(format!("spotify:{}:{id}", kind.as_str())));
        }
        LinkType::Steam => {
            let (_, app_id) = parse_steam_app_url(&link_info.url)?;
            return Some(ResourceKey(format!("steam:app:{app_id}")));
        }
        _ => return None,
    };
    let id = if provider == "seventv" {
        id.to_string()
    } else {
        id.to_ascii_lowercase()
    };
    Some(ResourceKey(format!("{provider}:{kind}:{id}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_image_detection() {
        let info = get_link_type("https://example.com/image.png");
        assert_eq!(info.link_type, LinkType::Image);

        let info = get_link_type("https://example.com/image.jpg?size=large");
        assert_eq!(info.link_type, LinkType::Image);
    }

    #[test]
    fn test_youtube_detection() {
        let info = get_link_type("https://www.youtube.com/watch?v=dQw4w9WgXcQ");
        assert_eq!(info.link_type, LinkType::YouTube);
        assert_eq!(info.video_id, Some("dQw4w9WgXcQ".to_string()));

        let info = get_link_type("https://youtu.be/dQw4w9WgXcQ");
        assert_eq!(info.link_type, LinkType::YouTube);
        assert_eq!(info.video_id, Some("dQw4w9WgXcQ".to_string()));
    }

    #[test]
    fn test_twitch_clip_detection() {
        let info = get_link_type("https://clips.twitch.tv/FunnyClipName123");
        assert_eq!(info.link_type, LinkType::TwitchClip);
        assert_eq!(info.clip_id, Some("FunnyClipName123".to_string()));
    }

    #[test]
    fn test_twitter_detection() {
        let info = get_link_type("https://twitter.com/user/status/123456789");
        assert_eq!(info.link_type, LinkType::Twitter);
        assert_eq!(info.tweet_id, Some("123456789".to_string()));

        let info = get_link_type("https://x.com/user/status/123456789");
        assert_eq!(info.link_type, LinkType::Twitter);
    }

    #[test]
    fn test_reddit_detection() {
        let info = get_link_type("https://reddit.com/r/programming/comments/abc123/title");
        assert_eq!(info.link_type, LinkType::Reddit);
        assert_eq!(info.subreddit, Some("programming".to_string()));
        assert_eq!(info.post_id, Some("abc123".to_string()));
    }
}
