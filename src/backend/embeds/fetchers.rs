//! Platform-specific metadata fetchers for embed previews
//!
//! Fetches metadata using oEmbed endpoints and predictable URL patterns
//! where available to minimize API dependencies.

use crate::backend::http;
use crate::backend::link_types::{
    EmbedLoadState, EmbedMetadata, LinkInfo, LinkType, MediaAsset, MediaKind, get_domain,
    get_link_type, get_youtube_thumbnail,
};
use crate::utils::truncate_text;
use reqwest::header::{CONTENT_LENGTH, CONTENT_TYPE};
use serde::Deserialize;
use std::sync::Arc;
use std::time::Duration;

/// HTTP client timeout for metadata fetches
const FETCH_TIMEOUT: Duration = Duration::from_secs(5);

/// Embed metadata fetcher service
pub struct EmbedFetcher;

/// Redirects are followed only to hosts `is_public_fetch_target` allows, and
/// host names connect only to the public addresses they resolve to (which
/// also defeats DNS rebinding). If the client cannot be built, embed fetches
/// fail closed instead of falling back to an unguarded client.
///
/// When a proxy is configured the proxy resolves the target host, so only the
/// URL and redirect checks apply; the address filter cannot see where the
/// proxy connects.
fn build_embed_http_client() -> reqwest::Client {
    http::client_builder()
        .timeout(FETCH_TIMEOUT)
        .connect_timeout(FETCH_TIMEOUT)
        .redirect(embed_redirect_policy())
        .dns_resolver(PublicOnlyResolver)
        .build()
        .unwrap_or_else(|error| {
            crate::log_net!(
                error,
                "Failed to build the embed HTTP client ({}); link previews will not fetch",
                error.without_url()
            );
            blocked_embed_client()
        })
}

pub(crate) fn embed_redirect_policy() -> reqwest::redirect::Policy {
    reqwest::redirect::Policy::custom(|attempt| {
        if attempt.previous().len() >= 10 {
            attempt.error("too many redirects")
        } else if is_public_fetch_target(attempt.url()) {
            attempt.follow()
        } else {
            attempt.stop()
        }
    })
}

/// A client whose every request fails locally: it only talks to an
/// unconnectable proxy (port 0 on loopback) and never follows redirects.
pub(crate) fn blocked_embed_client() -> reqwest::Client {
    let builder = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(FETCH_TIMEOUT);
    let builder = match reqwest::Proxy::all("http://127.0.0.1:0") {
        Ok(proxy) => builder.proxy(proxy),
        Err(_) => builder.no_proxy().connect_timeout(Duration::ZERO),
    };
    builder.build().unwrap_or_default()
}

/// DNS resolver for the embed client: drops every resolved address that is not
/// public, so a public-looking name cannot reach loopback or LAN hosts. The
/// configured proxy's own host is exempt, since a local proxy is legitimate.
pub(crate) struct PublicOnlyResolver;

impl reqwest::dns::Resolve for PublicOnlyResolver {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let host = name.as_str().to_string();
        let is_proxy_host = crate::backend::settings::get_proxy_enabled()
            && url::Url::parse(crate::backend::settings::get_proxy_url().trim())
                .ok()
                .and_then(|proxy| proxy.host_str().map(str::to_string))
                .is_some_and(|proxy_host| proxy_host.eq_ignore_ascii_case(&host));
        Box::pin(async move {
            let addrs: Vec<std::net::SocketAddr> = tokio::net::lookup_host((host.as_str(), 0))
                .await?
                .filter(|addr| is_proxy_host || is_public_ip(addr.ip()))
                .collect();
            if addrs.is_empty() {
                return Err(format!("{host} has no public address").into());
            }
            Ok(Box::new(addrs.into_iter()) as reqwest::dns::Addrs)
        })
    }
}

/// Whether an embed fetch or thumbnail download may contact this URL.
pub(crate) fn is_public_fetch_url(url: &str) -> bool {
    url::Url::parse(url).is_ok_and(|parsed| is_public_fetch_target(&parsed))
}

/// Whether an embed fetch may contact this URL: http(s) whose host is not a
/// local-only name (`localhost`, single-label, `.local`, `.lan`, `.internal`,
/// `.home.arpa`) or a non-public IP literal (see [`is_public_ip`]). Names are
/// checked again after resolution by [`PublicOnlyResolver`].
fn is_public_fetch_target(url: &url::Url) -> bool {
    if !matches!(url.scheme(), "http" | "https") {
        return false;
    }
    match url.host() {
        Some(url::Host::Domain(domain)) => is_public_host_name(domain),
        Some(url::Host::Ipv4(ip)) => is_public_ipv4(ip),
        Some(url::Host::Ipv6(ip)) => is_public_ipv6(ip),
        None => false,
    }
}

fn is_public_host_name(domain: &str) -> bool {
    let domain = domain.trim_end_matches('.').to_ascii_lowercase();
    const LOCAL_SUFFIXES: [&str; 5] = [".localhost", ".local", ".lan", ".internal", ".home.arpa"];
    domain.contains('.')
        && domain != "home.arpa"
        && !LOCAL_SUFFIXES.iter().any(|suffix| domain.ends_with(suffix))
}

fn is_public_ip(ip: std::net::IpAddr) -> bool {
    match ip {
        std::net::IpAddr::V4(ip) => is_public_ipv4(ip),
        std::net::IpAddr::V6(ip) => is_public_ipv6(ip),
    }
}

fn is_public_ipv4(ip: std::net::Ipv4Addr) -> bool {
    let [first, second, third, _] = ip.octets();
    let shared_cgnat = first == 100 && (second & 0xc0) == 64;
    let ietf_protocol = first == 192 && second == 0 && third == 0;
    let benchmarking = first == 198 && (second & 0xfe) == 18;
    let reserved = first >= 240;
    !(first == 0
        || ip.is_loopback()
        || ip.is_private()
        || ip.is_link_local()
        || ip.is_multicast()
        || shared_cgnat
        || ietf_protocol
        || benchmarking
        || reserved)
}

fn is_public_ipv6(ip: std::net::Ipv6Addr) -> bool {
    if let Some(mapped) = ip.to_ipv4_mapped() {
        return is_public_ipv4(mapped);
    }
    let segments = ip.segments();
    let embedded_low = || {
        let [.., a, b, c, d] = ip.octets();
        std::net::Ipv4Addr::new(a, b, c, d)
    };
    // IPv4-compatible `::a.b.c.d` (deprecated; also covers `::` and `::1`).
    if segments[..6].iter().all(|segment| *segment == 0) {
        return false;
    }
    // NAT64 well-known prefix 64:ff9b::/96 carries the IPv4 target in the low bits.
    if segments[..6] == [0x64, 0xff9b, 0, 0, 0, 0] {
        return is_public_ipv4(embedded_low());
    }
    // 6to4 2002::/16 carries the IPv4 relay in bits 16..48.
    if segments[0] == 0x2002 {
        let [_, _, a, b, c, d, ..] = ip.octets();
        return is_public_ipv4(std::net::Ipv4Addr::new(a, b, c, d));
    }
    let site_local = (segments[0] & 0xffc0) == 0xfec0;
    let documentation = segments[0] == 0x2001 && segments[1] == 0x0db8;
    !(ip.is_multicast()
        || ip.is_unique_local()
        || ip.is_unicast_link_local()
        || site_local
        || documentation)
}

static EMBED_HTTP_CLIENT: std::sync::LazyLock<http::ClientCache> =
    std::sync::LazyLock::new(|| http::ClientCache::new(build_embed_http_client));

impl EmbedFetcher {
    /// Create a metadata fetcher. The proxy-aware client is acquired per
    /// request from `ClientCache`; it is never permanently captured here.
    pub fn new() -> Self {
        Self
    }

    /// Fetch metadata for a URL
    pub async fn fetch(&self, url: &str) -> EmbedMetadata {
        let link_info = get_link_type(url);
        crate::log_net!(
            debug,
            "Processing URL: {} (type={:?})",
            url,
            link_info.link_type
        );

        let private_target = !is_public_fetch_url(&link_info.url);
        if !crate::backend::settings::get_embed_remote_fetch_enabled() || private_target {
            let metadata = self.create_privacy_safe_embed(&link_info);
            crate::log_net!(
                debug,
                "Direct embed metadata fetch disabled; cached safe preview for {}",
                url
            );
            return metadata;
        }

        crate::log_net!(
            info,
            "Starting HTTP fetch for: {} (type={:?})",
            url,
            link_info.link_type
        );
        let metadata = match link_info.link_type {
            LinkType::YouTube => self.fetch_youtube(&link_info).await,
            LinkType::TwitchClip => match link_info.clip_id.as_deref() {
                Some(clip_id) => {
                    super::twitch::fetch_clip(clip_id).into_metadata(link_info.clone())
                }
                None => self.create_failed_embed(&link_info),
            },
            LinkType::TwitchVod => match link_info.vod_id.as_deref() {
                Some(vod_id) => super::twitch::fetch_vod(&link_info.url, vod_id)
                    .await
                    .into_metadata(link_info.clone()),
                None => self.create_failed_embed(&link_info),
            },
            LinkType::TwitchChannel => match link_info.channel_login.as_deref() {
                Some(channel_login) => super::twitch::fetch_channel(&link_info.url, channel_login)
                    .await
                    .into_metadata(link_info.clone()),
                None => self.create_failed_embed(&link_info),
            },
            LinkType::SevenTvEmote => match link_info.seventv_emote_id.as_deref() {
                Some(emote_id) => {
                    super::seventv::fetch_seventv_emote_metadata(&link_info, emote_id).await
                }
                None => self.create_failed_embed(&link_info),
            },
            LinkType::SevenTvUser => match link_info.seventv_user_id.as_deref() {
                Some(user_id) => {
                    super::seventv::fetch_seventv_user_metadata(&link_info, user_id).await
                }
                None => self.create_failed_embed(&link_info),
            },
            LinkType::Spotify => {
                let Some((resource_kind, spotify_id)) =
                    super::spotify::parse_spotify_resource_url(&link_info.url)
                else {
                    return self.create_failed_embed(&link_info);
                };
                super::spotify::fetch_spotify_metadata(&link_info, resource_kind, &spotify_id).await
            }
            LinkType::Steam => {
                let Some((source, app_id)) = super::steam::parse_steam_app_url(&link_info.url)
                else {
                    return self.create_failed_embed(&link_info);
                };
                super::steam::fetch_steam_app_metadata(&link_info, &app_id, source.provider_label())
                    .await
            }
            LinkType::Twitter => self.fetch_twitter(&link_info).await,
            LinkType::Reddit => self.fetch_reddit(&link_info).await,
            LinkType::Image => self.fetch_image(&link_info).await,
            LinkType::Video => self.fetch_video(&link_info).await,
            LinkType::Generic => self.fetch_generic(&link_info).await,
        };

        crate::log_net!(
            info,
            "Fetch complete for {}: state={:?}, title={:?}",
            url,
            metadata.state,
            metadata.title
        );

        metadata
    }

    /// Fetch YouTube metadata via oEmbed
    async fn fetch_youtube(&self, link_info: &LinkInfo) -> EmbedMetadata {
        let video_id = match &link_info.video_id {
            Some(id) => id,
            None => {
                return self.create_failed_embed(link_info);
            }
        };

        let thumbnail_url = get_youtube_thumbnail(video_id);

        let oembed_url = format!(
            "https://www.youtube.com/oembed?url={}&format=json",
            urlencoding::encode(&link_info.url)
        );

        match EMBED_HTTP_CLIENT.get(&oembed_url).send().await {
            Ok(resp) if resp.status().is_success() => {
                if let Ok(data) = resp.json::<YouTubeOEmbed>().await {
                    return EmbedMetadata {
                        link_info: link_info.clone(),
                        thumbnail_url: Some(thumbnail_url),
                        title: Some(data.title),
                        description: None,
                        subtitle: Some(data.author_name),
                        duration: None,
                        content_type: None,
                        content_length: None,
                        media: None,
                        state: EmbedLoadState::Loaded,
                    };
                }
            }
            _ => {}
        }

        EmbedMetadata {
            link_info: link_info.clone(),
            thumbnail_url: Some(thumbnail_url),
            title: None,
            description: None,
            subtitle: Some("YouTube".to_string()),
            duration: None,
            content_type: None,
            content_length: None,
            media: None,
            state: EmbedLoadState::Loaded,
        }
    }

    /// Fetch Twitter/X metadata
    async fn fetch_twitter(&self, link_info: &LinkInfo) -> EmbedMetadata {
        if let Some(tweet_id) = link_info.tweet_id.as_deref() {
            let api_url = format!("https://api.fxtwitter.com/status/{tweet_id}");
            if let Ok(resp) = EMBED_HTTP_CLIENT.get(&api_url).send().await
                && resp.status().is_success()
                && let Ok(data) = resp.json::<FxTwitterResponse>().await
                && data.code == 200
                && let Some(tweet) = data.tweet
            {
                let title = tweet
                    .author
                    .as_ref()
                    .map(|author| format!("{} (@{})", author.name, author.screen_name))
                    .unwrap_or_else(|| "Twitter/X".to_string());
                let subtitle = {
                    let text = tweet.text.trim();
                    (!text.is_empty()).then(|| crate::utils::truncate_text(text, 200))
                };
                let thumbnail_url = tweet.media.as_ref().and_then(|media| {
                    media
                        .photos
                        .as_ref()
                        .and_then(|photos| photos.first())
                        .map(|photo| photo.url.clone())
                        .or_else(|| {
                            media
                                .videos
                                .as_ref()
                                .and_then(|videos| videos.first())
                                .and_then(|video| video.thumbnail_url.clone())
                        })
                });
                return EmbedMetadata {
                    link_info: link_info.clone(),
                    thumbnail_url,
                    title: Some(title),
                    description: None,
                    subtitle: subtitle.or_else(|| Some("Twitter/X".to_string())),
                    duration: None,
                    content_type: None,
                    content_length: None,
                    media: None,
                    state: EmbedLoadState::Loaded,
                };
            }
        }

        EmbedMetadata {
            link_info: link_info.clone(),
            thumbnail_url: None,
            title: None,
            description: None,
            subtitle: Some("Twitter/X".to_string()),
            duration: None,
            content_type: None,
            content_length: None,
            media: None,
            state: EmbedLoadState::Loaded,
        }
    }

    /// Fetch Reddit metadata via JSON API
    async fn fetch_reddit(&self, link_info: &LinkInfo) -> EmbedMetadata {
        let json_url =
            reddit_json_url(&link_info.url).unwrap_or_else(|| format!("{}.json", link_info.url));

        match EMBED_HTTP_CLIENT.get(&json_url).send().await {
            Ok(resp) if resp.status().is_success() => {
                if let Ok(data) = resp.json::<serde_json::Value>().await
                    && let Some(post) = data
                        .get(0)
                        .and_then(|l| l.get("data"))
                        .and_then(|d| d.get("children"))
                        .and_then(|c| c.get(0))
                        .and_then(|p| p.get("data"))
                {
                    let title = post
                        .get("title")
                        .and_then(|t| t.as_str())
                        .map(|s| s.to_string());
                    let subreddit = post
                        .get("subreddit_name_prefixed")
                        .and_then(|s| s.as_str())
                        .map(|s| s.to_string());
                    let thumbnail = post
                        .get("thumbnail")
                        .and_then(|t| t.as_str())
                        .filter(|t| t.starts_with("http"))
                        .map(|s| s.to_string())
                        .or_else(|| {
                            post.get("preview")
                                .and_then(|preview| preview.get("images"))
                                .and_then(|images| images.as_array())
                                .and_then(|images| images.first())
                                .and_then(|image| image.get("source"))
                                .and_then(|source| source.get("url"))
                                .and_then(|url| url.as_str())
                                .map(decode_common_html_entities)
                        });

                    return EmbedMetadata {
                        link_info: link_info.clone(),
                        thumbnail_url: thumbnail,
                        title,
                        description: None,
                        subtitle: subreddit,
                        duration: None,
                        content_type: None,
                        content_length: None,
                        media: None,
                        state: EmbedLoadState::Loaded,
                    };
                }
            }
            _ => {}
        }

        EmbedMetadata {
            link_info: link_info.clone(),
            thumbnail_url: None,
            title: None,
            description: None,
            subtitle: link_info.subreddit.as_ref().map(|s| format!("r/{}", s)),
            duration: None,
            content_type: None,
            content_length: None,
            media: None,
            state: EmbedLoadState::Loaded,
        }
    }

    async fn fetch_image(&self, link_info: &LinkInfo) -> EmbedMetadata {
        let response = match EMBED_HTTP_CLIENT.head(&link_info.url).send().await {
            Ok(resp) if resp.status().is_success() => Some(resp),
            _ => None,
        };

        let Some(response) = response else {
            return self.create_image_embed(link_info);
        };
        let (content_type, content_length) = response_content_headers(&response);
        let mut metadata = self.create_image_embed(link_info);
        metadata.content_type = content_type;
        metadata.content_length = content_length;
        metadata
    }

    /// Fetch direct-video response headers only. We deliberately do not read
    /// its body: the preview is static metadata/poster art, never playback.
    async fn fetch_video(&self, link_info: &LinkInfo) -> EmbedMetadata {
        let response = match EMBED_HTTP_CLIENT.head(&link_info.url).send().await {
            Ok(resp) if resp.status().is_success() => Some(resp),
            _ => None,
        };

        let Some(response) = response else {
            return self.create_video_embed(link_info, None);
        };
        let (content_type, content_length) = response_content_headers(&response);
        self.create_video_embed(link_info, Some((content_type, content_length)))
    }

    /// Create embed for direct image URLs
    fn create_image_embed(&self, link_info: &LinkInfo) -> EmbedMetadata {
        EmbedMetadata {
            link_info: link_info.clone(),
            thumbnail_url: Some(link_info.url.clone()),
            title: None,
            description: None,
            subtitle: link_info
                .provider_name
                .clone()
                .or_else(|| Some("Image".to_string())),
            duration: None,
            content_type: None,
            content_length: None,
            media: Some(MediaAsset {
                kind: MediaKind::Image,
                source_url: link_info.url.clone(),
                poster_url: Some(link_info.url.clone()),
            }),
            state: EmbedLoadState::Loaded,
        }
    }

    /// Create a direct-video preview. `LinkInfo::url` stays the browser-open
    /// target; the media asset exists only to label it as static video.
    fn create_video_embed(
        &self,
        link_info: &LinkInfo,
        headers: Option<(Option<String>, Option<u64>)>,
    ) -> EmbedMetadata {
        let (content_type, content_length) = headers.unwrap_or((None, None));
        EmbedMetadata {
            link_info: link_info.clone(),
            thumbnail_url: None,
            title: None,
            description: None,
            subtitle: link_info
                .provider_name
                .clone()
                .or_else(|| Some("Video".to_string())),
            duration: None,
            content_type,
            content_length,
            media: Some(MediaAsset {
                kind: MediaKind::Video,
                source_url: link_info.url.clone(),
                poster_url: None,
            }),
            state: EmbedLoadState::Loaded,
        }
    }

    /// Fetch generic URL metadata via Open Graph tags
    ///
    /// Parses HTML to extract:
    /// - og:title / title tag
    /// - og:image / twitter:image
    /// - og:description / meta description
    /// - og:site_name
    async fn fetch_generic(&self, link_info: &LinkInfo) -> EmbedMetadata {
        match EMBED_HTTP_CLIENT.get(&link_info.url).send().await {
            Ok(resp) if resp.status().is_success() => {
                let (content_type, content_length) = response_content_headers(&resp);
                let content_type_header = content_type.as_deref().unwrap_or("");

                if content_type_header
                    .to_ascii_lowercase()
                    .starts_with("image/")
                {
                    let mut metadata = self.create_image_embed(link_info);
                    metadata.content_type = content_type;
                    metadata.content_length = content_length;
                    return metadata;
                }

                if content_type_header
                    .to_ascii_lowercase()
                    .starts_with("video/")
                {
                    return self.create_video_embed(
                        &LinkInfo {
                            link_type: LinkType::Video,
                            ..link_info.clone()
                        },
                        Some((content_type, content_length)),
                    );
                }

                if !is_html_content_type(content_type_header) {
                    return self.create_basic_embed(link_info);
                }

                const MAX_BODY_SIZE: usize = 512 * 1024;
                let mut resp = resp;
                let mut buf: Vec<u8> = Vec::with_capacity(64 * 1024);
                loop {
                    match resp.chunk().await {
                        Ok(Some(chunk)) => {
                            if chunk.is_empty() {
                                break;
                            }
                            let room = MAX_BODY_SIZE - buf.len();
                            let take = chunk.len().min(room);
                            buf.extend_from_slice(&chunk[..take]);
                            if buf.len() >= MAX_BODY_SIZE {
                                break;
                            }
                        }
                        Ok(None) => break,
                        Err(_) => break,
                    }
                }
                if !buf.is_empty() {
                    let html = String::from_utf8_lossy(&buf);
                    return self.parse_opengraph(&html, link_info, content_type, content_length);
                }
            }
            _ => {}
        }

        self.create_basic_embed(link_info)
    }

    /// Parse OpenGraph metadata from HTML
    fn parse_opengraph(
        &self,
        html: &str,
        link_info: &LinkInfo,
        content_type: Option<String>,
        content_length: Option<u64>,
    ) -> EmbedMetadata {
        use scraper::{Html, Selector};

        let document = Html::parse_document(html);

        let og_title_sel = Selector::parse("meta[property='og:title']").ok();
        let og_image_sel = Selector::parse("meta[property='og:image']").ok();
        let og_video_secure_sel = Selector::parse("meta[property='og:video:secure_url']").ok();
        let og_video_sel = Selector::parse("meta[property='og:video']").ok();
        let og_desc_sel = Selector::parse("meta[property='og:description']").ok();
        let og_site_sel = Selector::parse("meta[property='og:site_name']").ok();
        let twitter_title_sel = Selector::parse("meta[name='twitter:title']").ok();
        let twitter_site_sel = Selector::parse("meta[name='twitter:site']").ok();
        let twitter_image_sel = Selector::parse("meta[name='twitter:image']").ok();
        let title_sel = Selector::parse("title").ok();
        let desc_sel = Selector::parse("meta[name='description']").ok();
        let application_name_sel = Selector::parse("meta[name='application-name']").ok();

        let title: Option<String> = og_title_sel
            .and_then(|s| document.select(&s).next())
            .and_then(|e| e.value().attr("content"))
            .map(|s| truncate_text(s, 100))
            .or_else(|| {
                twitter_title_sel
                    .and_then(|s| document.select(&s).next())
                    .and_then(|e| e.value().attr("content"))
                    .map(|s| truncate_text(s, 100))
            })
            .or_else(|| {
                title_sel
                    .and_then(|s| document.select(&s).next())
                    .map(|e| e.text().collect::<String>())
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .map(|s| truncate_text(&s, 100))
            });

        let image = og_image_sel
            .and_then(|s| document.select(&s).next())
            .and_then(|e| e.value().attr("content"))
            .or_else(|| {
                twitter_image_sel
                    .and_then(|s| document.select(&s).next())
                    .and_then(|e| e.value().attr("content"))
            })
            .and_then(|s| resolve_embed_url(&link_info.url, s));

        let video = og_video_secure_sel
            .and_then(|s| document.select(&s).next())
            .and_then(|e| e.value().attr("content"))
            .or_else(|| {
                og_video_sel
                    .and_then(|s| document.select(&s).next())
                    .and_then(|e| e.value().attr("content"))
            })
            .and_then(|s| resolve_embed_url(&link_info.url, s));

        let site_name = og_site_sel
            .and_then(|s| document.select(&s).next())
            .and_then(|e| e.value().attr("content"))
            .map(String::from)
            .or_else(|| {
                twitter_site_sel
                    .and_then(|s| document.select(&s).next())
                    .and_then(|e| e.value().attr("content"))
                    .map(|site| site.trim_start_matches('@').to_string())
            })
            .or_else(|| {
                application_name_sel
                    .and_then(|s| document.select(&s).next())
                    .and_then(|e| e.value().attr("content"))
                    .map(String::from)
            })
            .or_else(|| link_info.provider_name.clone())
            .or_else(|| Some(get_domain(&link_info.url)));

        let description = og_desc_sel
            .and_then(|s| document.select(&s).next())
            .and_then(|e| e.value().attr("content"))
            .or_else(|| {
                desc_sel
                    .and_then(|s| document.select(&s).next())
                    .and_then(|e| e.value().attr("content"))
            })
            .map(|text| truncate_text(text.trim(), 100))
            .filter(|text| !text.is_empty());

        EmbedMetadata {
            link_info: link_info.clone(),
            thumbnail_url: image.clone(),
            title,
            description,
            subtitle: site_name,
            duration: None,
            content_type,
            content_length,
            media: video
                .map(|source_url| MediaAsset {
                    kind: MediaKind::Video,
                    source_url,
                    poster_url: image.clone(),
                })
                .or_else(|| {
                    image.clone().map(|source_url| MediaAsset {
                        kind: MediaKind::Image,
                        poster_url: Some(source_url.clone()),
                        source_url,
                    })
                }),
            state: EmbedLoadState::Loaded,
        }
    }

    /// Create a basic embed without fetching
    fn create_basic_embed(&self, link_info: &LinkInfo) -> EmbedMetadata {
        EmbedMetadata {
            link_info: link_info.clone(),
            thumbnail_url: None,
            title: None,
            description: None,
            subtitle: link_info
                .provider_name
                .clone()
                .or_else(|| Some(get_domain(&link_info.url))),
            duration: None,
            content_type: None,
            content_length: None,
            media: None,
            state: EmbedLoadState::Loaded,
        }
    }

    /// Create a preview that never requires Echo to contact the linked site.
    fn create_privacy_safe_embed(&self, link_info: &LinkInfo) -> EmbedMetadata {
        let subtitle = match link_info.link_type {
            LinkType::YouTube => Some("YouTube".to_string()),
            LinkType::TwitchClip => Some("Twitch Clip".to_string()),
            LinkType::TwitchVod => Some("Twitch VOD".to_string()),
            LinkType::TwitchChannel => Some("Twitch".to_string()),
            LinkType::SevenTvEmote => Some("7TV emote".to_string()),
            LinkType::SevenTvUser => Some("7TV user".to_string()),
            LinkType::Spotify => super::spotify::parse_spotify_resource_url(&link_info.url)
                .map(|(kind, _)| format!("Spotify {}", kind.as_str()))
                .or_else(|| Some("Spotify".to_string())),
            LinkType::Steam => super::steam::parse_steam_app_url(&link_info.url)
                .map(|(source, _)| format!("{} game", source.provider_label()))
                .or_else(|| Some("Steam game".to_string())),
            LinkType::Twitter => Some("Twitter/X".to_string()),
            LinkType::Reddit => link_info
                .subreddit
                .as_ref()
                .map(|s| format!("r/{}", s))
                .or_else(|| Some("Reddit".to_string())),
            LinkType::Image => link_info
                .provider_name
                .clone()
                .or_else(|| Some("Image".to_string())),
            LinkType::Video => link_info
                .provider_name
                .clone()
                .or_else(|| Some("Video".to_string())),
            LinkType::Generic => link_info
                .provider_name
                .clone()
                .or_else(|| Some(get_domain(&link_info.url))),
        };

        EmbedMetadata {
            link_info: link_info.clone(),
            thumbnail_url: None,
            title: None,
            description: None,
            subtitle,
            duration: None,
            content_type: None,
            content_length: None,
            media: None,
            state: EmbedLoadState::Loaded,
        }
    }

    /// Create a failed embed metadata
    fn create_failed_embed(&self, link_info: &LinkInfo) -> EmbedMetadata {
        EmbedMetadata {
            link_info: link_info.clone(),
            thumbnail_url: None,
            title: None,
            description: None,
            subtitle: None,
            duration: None,
            content_type: None,
            content_length: None,
            media: None,
            state: EmbedLoadState::Failed,
        }
    }
}

impl Default for EmbedFetcher {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Deserialize)]
struct YouTubeOEmbed {
    title: String,
    author_name: String,
}

#[derive(Debug, Deserialize)]
struct FxTwitterResponse {
    #[serde(default)]
    code: u32,
    #[serde(default)]
    tweet: Option<FxTweet>,
}

#[derive(Debug, Deserialize)]
struct FxTweet {
    #[serde(default)]
    text: String,
    #[serde(default)]
    author: Option<FxAuthor>,
    #[serde(default)]
    media: Option<FxMedia>,
}

#[derive(Debug, Deserialize)]
struct FxAuthor {
    #[serde(default)]
    name: String,
    #[serde(default)]
    screen_name: String,
}

#[derive(Debug, Deserialize)]
struct FxMedia {
    #[serde(default)]
    photos: Option<Vec<FxPhoto>>,
    #[serde(default)]
    videos: Option<Vec<FxVideo>>,
}

#[derive(Debug, Deserialize)]
struct FxPhoto {
    #[serde(default)]
    url: String,
}

#[derive(Debug, Deserialize)]
struct FxVideo {
    #[serde(default)]
    thumbnail_url: Option<String>,
}

use std::sync::LazyLock;

/// Global embed fetcher instance
static FETCHER: LazyLock<Arc<EmbedFetcher>> = LazyLock::new(|| Arc::new(EmbedFetcher::new()));

/// Fetch embed metadata for a URL (uses global fetcher)
pub async fn fetch_embed_metadata(url: &str) -> EmbedMetadata {
    FETCHER.fetch(url).await
}

/// Construct the terminal failure result used when the flight-level timeout
/// wins before the provider fetcher can return one.
pub fn failed_embed_metadata(url: &str) -> EmbedMetadata {
    let link_info = get_link_type(url);
    EmbedMetadata {
        link_info,
        thumbnail_url: None,
        title: None,
        description: None,
        subtitle: None,
        duration: None,
        content_type: None,
        content_length: None,
        media: None,
        state: EmbedLoadState::Failed,
    }
}

fn response_content_headers(response: &reqwest::Response) -> (Option<String>, Option<u64>) {
    let content_type = response
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let content_length = response
        .headers()
        .get(CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok());
    (content_type, content_length)
}

fn is_html_content_type(content_type: &str) -> bool {
    let lowered = content_type.to_ascii_lowercase();
    lowered.contains("text/html") || lowered.contains("application/xhtml+xml")
}

fn resolve_embed_url(base_url: &str, value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    if value.starts_with("http://") || value.starts_with("https://") {
        return Some(value.to_string());
    }
    if value.starts_with("//") {
        return Some(format!("https:{value}"));
    }

    let base = url::Url::parse(base_url).ok()?;
    base.join(value).ok().map(|url| url.to_string())
}

fn reddit_json_url(url: &str) -> Option<String> {
    let mut parsed = url::Url::parse(url).ok()?;
    let new_path = if parsed.path().ends_with(".json") {
        parsed.path().to_string()
    } else {
        format!("{}.json", parsed.path().trim_end_matches('/'))
    };
    parsed.set_path(&new_path);
    Some(parsed.to_string())
}

fn decode_common_html_entities(value: &str) -> String {
    value
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
}

#[cfg(test)]
mod tests {
    use super::is_public_fetch_target;
    use core::prelude::v1::test;

    fn allowed(url: &str) -> bool {
        is_public_fetch_target(&url::Url::parse(url).unwrap())
    }

    #[test]
    fn embed_fetches_skip_local_and_private_targets() {
        assert!(allowed("https://example.com/page"));
        assert!(allowed("http://8.8.8.8/"));
        for blocked in [
            "http://localhost:8080/",
            "http://api.localhost/",
            "http://127.0.0.1/",
            "http://2130706433/",
            "http://10.0.0.1/",
            "http://192.168.1.1/admin",
            "http://169.254.169.254/latest/meta-data",
            "http://100.64.0.1/",
            "http://0.0.0.0/",
            "http://[::1]/",
            "http://[fe80::1]/",
            "http://[fd00::1]/",
            "http://[::ffff:127.0.0.1]/",
            "file:///etc/passwd",
            "http://[::127.0.0.1]/",
            "http://[::a00:1]/",
            "http://[64:ff9b::a00:1]/",
            "http://[64:ff9b::7f00:1]/",
            "http://[2002:c0a8:101::1]/",
            "http://[2002:7f00:1::]/",
            "http://[fec0::1]/",
            "http://[2001:db8::1]/",
            "http://198.18.0.1/",
            "http://198.19.255.255/",
            "http://240.0.0.1/",
            "http://255.255.255.255/",
            "http://192.0.0.8/",
            "http://printer.local/",
            "http://nas.lan/",
            "http://metadata.google.internal/",
            "http://router.home.arpa/",
            "http://home.arpa/",
            "http://intranet/",
            "http://intranet./",
        ] {
            assert!(!allowed(blocked), "{blocked}");
        }
    }

    #[test]
    fn embed_fetches_allow_public_v6_and_embedded_public_v4() {
        for public in [
            "http://[2606:4700::1111]/",
            "http://[64:ff9b::808:808]/",
            "http://[2002:808:808::1]/",
            "http://198.20.0.1/",
            "http://192.0.1.1/",
            "http://sub.example.co.uk./",
        ] {
            assert!(allowed(public), "{public}");
        }
    }
}
