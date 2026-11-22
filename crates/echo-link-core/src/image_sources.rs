//! Full-size renditions of the image URLs link previews carry.
//!
//! A preview card's image is often a reduced rendition the page or provider
//! chose for cards (a Wikimedia thumbnail, a 300 px Spotify cover, a Twitch
//! live preview at 640x360). The image preview window wants the original.
//! These rules only rewrite URL families whose full-size form is documented
//! by the URL scheme itself; the caller tries each candidate in order and
//! falls back to the advertised URL, so a wrong guess costs one request.

use url::Url;

/// Largest Twitch live-preview rendition: the stream's own 1080p frame.
pub const TWITCH_LIVE_PREVIEW_FULL_SIZE: (u32, u32) = (1920, 1080);

/// Raster formats the preview window decodes. A Wikimedia thumbnail of any
/// other original (SVG, TIFF, PDF, video) stays the best decodable rendition.
const DECODABLE_EXTENSIONS: &[&str] = &["jpg", "jpeg", "png", "gif", "webp"];

/// Spotify cover-art size prefixes, smallest first, followed by the larger
/// renditions to try in order. The 40-hex image id keeps its tail across sizes.
const SPOTIFY_ALBUM_SMALL: &[&str] = &["ab67616d00004851", "ab67616d00001e02", "ab67616d0000b273"];
const SPOTIFY_ALBUM_LARGE: &[&str] = &["ab67616d000082c1", "ab67616d0000b273"];
const SPOTIFY_ARTIST_SMALL: &[&str] = &["ab6761610000f178", "ab67616100005174"];
const SPOTIFY_ARTIST_LARGE: &[&str] = &["ab6761610000e5eb"];

/// URLs to try, best first, before falling back to `source` itself. Empty when
/// `source` is already the original or no larger rendition is known.
pub fn full_size_image_candidates(source: &str) -> Vec<String> {
    let Ok(url) = Url::parse(source.trim()) else {
        return Vec::new();
    };
    if url.scheme() != "https" {
        return Vec::new();
    }
    let Some(host) = url.host_str().map(str::to_ascii_lowercase) else {
        return Vec::new();
    };
    let candidates = match host.as_str() {
        "upload.wikimedia.org" | "thumb.wikimedia.org" => {
            wikimedia_original(&url).into_iter().collect()
        }
        "i.scdn.co" => spotify_larger(&url),
        host if host.ends_with(".spotifycdn.com") => spotify_larger(&url),
        "pbs.twimg.com" => twitter_original(&url).into_iter().collect(),
        "media.discordapp.net" => discord_original(&url).into_iter().collect(),
        "static-cdn.jtvnw.net" => twitch_live_preview_full(&url).into_iter().collect(),
        "i.ytimg.com" | "img.youtube.com" => youtube_larger(&url),
        _ => Vec::new(),
    };
    candidates
        .into_iter()
        .filter(|candidate: &String| candidate != source)
        .collect()
}

/// `/<project>/thumb/<a>/<ab>/<File>/<size>px-<File>[.ext]` becomes
/// `https://upload.wikimedia.org/<project>/<a>/<ab>/<File>`.
fn wikimedia_original(url: &Url) -> Option<String> {
    let segments: Vec<&str> = url.path_segments()?.collect();
    let thumb = segments.iter().position(|segment| *segment == "thumb")?;
    let [shard, shard2, file, _rendition] = segments.get(thumb + 1..)? else {
        return None;
    };
    let is_hex = |value: &str| value.bytes().all(|byte| byte.is_ascii_hexdigit());
    if shard.len() != 1 || shard2.len() != 2 || !is_hex(shard) || !shard2.starts_with(shard) {
        return None;
    }
    if !is_hex(shard2) || thumb == 0 {
        return None;
    }
    let extension = file.rsplit_once('.')?.1.to_ascii_lowercase();
    if !DECODABLE_EXTENSIONS.contains(&extension.as_str()) {
        return None;
    }
    let project = segments[..thumb].join("/");
    Some(format!(
        "https://upload.wikimedia.org/{project}/{shard}/{shard2}/{file}"
    ))
}

/// Spotify `/image/<size prefix><id tail>`: larger cover or artist renditions.
fn spotify_larger(url: &Url) -> Vec<String> {
    let segments: Option<Vec<&str>> = url.path_segments().map(Iterator::collect);
    let Some(["image", image_id]) = segments.as_deref() else {
        return Vec::new();
    };
    if image_id.len() != 40 || !image_id.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Vec::new();
    }
    let (prefix, tail) = image_id.split_at(16);
    let larger = if SPOTIFY_ALBUM_SMALL.contains(&prefix) {
        SPOTIFY_ALBUM_LARGE
    } else if SPOTIFY_ARTIST_SMALL.contains(&prefix) {
        SPOTIFY_ARTIST_LARGE
    } else {
        return Vec::new();
    };
    let origin = url.origin().ascii_serialization();
    larger
        .iter()
        .filter(|candidate| **candidate != prefix)
        .map(|candidate| format!("{origin}/image/{candidate}{tail}"))
        .collect()
}

/// `pbs.twimg.com/media/<id>?format=jpg&name=small` asks for `name=orig`.
fn twitter_original(url: &Url) -> Option<String> {
    if !url.path().starts_with("/media/") {
        return None;
    }
    let name = url
        .query_pairs()
        .find_map(|(key, value)| (key == "name").then(|| value.into_owned()))?;
    if name == "orig" {
        return None;
    }
    let mut original = url.clone();
    let pairs: Vec<(String, String)> = url
        .query_pairs()
        .map(|(key, value)| {
            let value = if key == "name" {
                "orig".into()
            } else {
                value.into_owned()
            };
            (key.into_owned(), value)
        })
        .collect();
    original.query_pairs_mut().clear().extend_pairs(pairs);
    Some(original.into())
}

/// Discord's media proxy resizes attachments to its `width`/`height` query;
/// without them it serves the attachment at its own size. The signed `ex`,
/// `is` and `hm` parameters stay.
fn discord_original(url: &Url) -> Option<String> {
    if !url.path().starts_with("/attachments/") {
        return None;
    }
    let is_size_key = |key: &str| matches!(key, "width" | "height");
    if !url.query_pairs().any(|(key, _)| is_size_key(&key)) {
        return None;
    }
    let pairs: Vec<(String, String)> = url
        .query_pairs()
        .filter(|(key, _)| !is_size_key(key))
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect();
    let mut original = url.clone();
    if pairs.is_empty() {
        original.set_query(None);
    } else {
        original.query_pairs_mut().clear().extend_pairs(pairs);
    }
    Some(original.into())
}

/// Parsed `previews-ttv/live_user_<login>-<w>x<h>.<ext>` (or the
/// `{width}x{height}` template form).
pub(crate) struct TwitchLivePreview<'a> {
    pub login: &'a str,
    pub size: Option<(u32, u32)>,
    pub extension: &'a str,
}

pub(crate) fn parse_twitch_live_preview(url: &Url) -> Option<TwitchLivePreview<'_>> {
    if !url
        .host_str()
        .is_some_and(|host| host.eq_ignore_ascii_case("static-cdn.jtvnw.net"))
    {
        return None;
    }
    let mut segments = url.path_segments()?;
    if segments.next()? != "previews-ttv" {
        return None;
    }
    let file = segments.next()?;
    if segments.next().is_some() {
        return None;
    }
    let (stem, extension) = file.rsplit_once('.')?;
    let rest = stem.strip_prefix("live_user_")?;
    let (login, size) = rest.rsplit_once('-')?;
    if login.is_empty()
        || !login
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
    {
        return None;
    }
    let size = if size == "{width}x{height}" || size == "%7Bwidth%7Dx%7Bheight%7D" {
        None
    } else {
        let (width, height) = size.split_once('x')?;
        Some((width.parse().ok()?, height.parse().ok()?))
    };
    Some(TwitchLivePreview {
        login,
        size,
        extension,
    })
}

/// The 1920x1080 rendition of a Twitch live preview for `login`.
pub fn twitch_live_preview_url(login: &str, extension: &str) -> String {
    let (width, height) = TWITCH_LIVE_PREVIEW_FULL_SIZE;
    format!(
        "https://static-cdn.jtvnw.net/previews-ttv/live_user_{}-{width}x{height}.{extension}",
        login.to_ascii_lowercase()
    )
}

fn twitch_live_preview_full(url: &Url) -> Option<String> {
    let preview = parse_twitch_live_preview(url)?;
    (preview.size != Some(TWITCH_LIVE_PREVIEW_FULL_SIZE))
        .then(|| twitch_live_preview_url(preview.login, preview.extension))
}

/// `/vi/<id>/<rendition>.jpg`: the largest YouTube thumbnails, best first.
/// `maxresdefault` exists only for HD uploads; the caller falls back.
fn youtube_larger(url: &Url) -> Vec<String> {
    let segments: Option<Vec<&str>> = url.path_segments().map(Iterator::collect);
    let Some(["vi", video_id, file]) = segments.as_deref() else {
        return Vec::new();
    };
    if !crate::is_youtube_video_id(video_id)
        || !matches!(
            *file,
            "default.jpg" | "mqdefault.jpg" | "hqdefault.jpg" | "sddefault.jpg"
        )
    {
        return Vec::new();
    }
    ["maxresdefault.jpg", "sddefault.jpg", "hqdefault.jpg"]
        .iter()
        .take_while(|larger| **larger != *file)
        .map(|larger| format!("https://i.ytimg.com/vi/{video_id}/{larger}"))
        .collect()
}

/// Which picture a link's preview card would show, as far as the link itself
/// says, so two links in one message that would show the same picture get one
/// card. Links of the same picture share a key:
///
/// - a Twitch channel link and a live preview image of that channel, at any
///   size, in the `{width}x{height}` template form, or with a query;
/// - a YouTube video link and that video's thumbnail images;
/// - otherwise only the same URL (ignoring a fragment).
///
/// The key depends on the URL alone, never on fetched metadata, so which card
/// a message keeps cannot change as metadata arrives.
pub fn card_picture_key(url: &str) -> String {
    let trimmed = url.trim();
    let with_scheme;
    let url = if trimmed.contains("://") {
        trimmed
    } else {
        with_scheme = format!("https://{trimmed}");
        &with_scheme
    };
    let link = crate::get_link_type(url);
    match link.link_type {
        crate::LinkType::TwitchChannel => {
            if let Some(login) = link.channel_login {
                return format!("twitch-live:{}", login.to_ascii_lowercase());
            }
        }
        crate::LinkType::YouTube => {
            if let Some(video_id) = link.video_id {
                return format!("youtube:{video_id}");
            }
        }
        _ => {}
    }
    let Ok(mut parsed) = Url::parse(url) else {
        return format!("url:{trimmed}");
    };
    if let Some(preview) = parse_twitch_live_preview(&parsed) {
        return format!("twitch-live:{}", preview.login.to_ascii_lowercase());
    }
    if let Some(video_id) = youtube_thumbnail_video_id(&parsed) {
        return format!("youtube:{video_id}");
    }
    parsed.set_fragment(None);
    format!("url:{parsed}")
}

/// Whether a link keeps its claim on `picture_key` (its [`card_picture_key`]),
/// so a later link to that picture in the same message gets no card.
///
/// `card_image` is `None` while the link's card metadata is pending (the claim
/// holds, so the later card never appears and then vanishes), and once it has
/// resolved, the image the card shows, if any. A card without an image gives
/// the picture up. A Twitch channel or YouTube link keeps it only when its card
/// really shows that picture: a channel card that fell back to the avatar
/// (no stream yet, a failed lookup, the public page) must not hide the
/// stream frame a later preview link shows.
pub fn link_keeps_picture_claim(picture_key: &str, card_image: Option<Option<&str>>) -> bool {
    match card_image {
        None => true,
        Some(None) => false,
        Some(Some(image)) => {
            let names_a_family =
                picture_key.starts_with("twitch-live:") || picture_key.starts_with("youtube:");
            !names_a_family || card_picture_key(image) == picture_key
        }
    }
}

/// The video id of an `i.ytimg.com`/`img.youtube.com` `/vi/<id>/<file>` (or
/// `/vi_webp/`) thumbnail.
fn youtube_thumbnail_video_id(url: &Url) -> Option<String> {
    let host = url.host_str()?.to_ascii_lowercase();
    if host != "i.ytimg.com" && host != "img.youtube.com" {
        return None;
    }
    let segments: Vec<&str> = url.path_segments()?.collect();
    match segments.as_slice() {
        ["vi" | "vi_webp", video_id, _file] if !video_id.is_empty() => {
            Some((*video_id).to_string())
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIVE_LINE: &str = "https://twitch.tv/somechannel";
    const LIVE_PREVIEW: &str =
        "https://static-cdn.jtvnw.net/previews-ttv/live_user_somechannel-1920x1080.jpg";

    #[test]
    fn the_bots_live_line_names_one_picture_twice() {
        // `-p somechannel` for a live channel: the channel link first, the
        // stream's 1080p preview last.
        assert_eq!(card_picture_key(LIVE_LINE), card_picture_key(LIVE_PREVIEW));
        assert_eq!(
            card_picture_key("twitch.tv/somechannel"),
            card_picture_key(LIVE_PREVIEW)
        );
    }

    #[test]
    fn sized_and_template_previews_match_the_channel_in_either_order() {
        for preview in [
            "https://static-cdn.jtvnw.net/previews-ttv/live_user_somechannel-640x360.jpg",
            "https://static-cdn.jtvnw.net/previews-ttv/live_user_SomeChannel-{width}x{height}.jpg",
            "https://static-cdn.jtvnw.net/previews-ttv/live_user_somechannel-1280x720.jpg?t=1700000000",
        ] {
            assert_eq!(
                card_picture_key(preview),
                card_picture_key(LIVE_PREVIEW),
                "{preview}"
            );
            assert_eq!(
                card_picture_key(LIVE_LINE),
                card_picture_key(preview),
                "{preview}"
            );
        }
        assert_eq!(
            card_picture_key("https://www.twitch.tv/SomeChannel"),
            card_picture_key(LIVE_LINE)
        );
    }

    #[test]
    fn different_logins_and_unrelated_images_stay_apart() {
        assert_ne!(
            card_picture_key("https://twitch.tv/otherchannel"),
            card_picture_key(LIVE_PREVIEW)
        );
        assert_ne!(
            card_picture_key(LIVE_LINE),
            card_picture_key("https://upload.wikimedia.org/wikipedia/commons/b/b6/Cat.jpg")
        );
        // A clip or VOD is not the channel's live picture.
        assert_ne!(
            card_picture_key("https://www.twitch.tv/videos/123456"),
            card_picture_key(LIVE_LINE)
        );
    }

    #[test]
    fn identical_links_share_a_key_and_different_pictures_do_not() {
        let cat = "https://upload.wikimedia.org/wikipedia/commons/b/b6/Cat.jpg";
        assert_eq!(card_picture_key(cat), card_picture_key(cat));
        assert_eq!(
            card_picture_key(cat),
            card_picture_key(&format!("{cat}#top"))
        );
        assert_ne!(
            card_picture_key(cat),
            card_picture_key("https://upload.wikimedia.org/wikipedia/commons/2/2c/Earth.gif")
        );
        // The offline line has one link: nothing to pair it with.
        assert_eq!(
            card_picture_key("twitch.tv/somechannel"),
            "twitch-live:somechannel"
        );
    }

    #[test]
    fn a_channel_link_keeps_its_claim_only_while_its_card_could_show_the_stream() {
        let key = card_picture_key(LIVE_LINE);
        // Pending: the claim holds, so the later preview card never flickers in.
        assert!(link_keeps_picture_claim(&key, None));
        // Resolved to the live preview (any size): it keeps the claim.
        assert!(link_keeps_picture_claim(
            &key,
            Some(Some(
                "https://static-cdn.jtvnw.net/previews-ttv/live_user_somechannel-640x360.jpg"
            ))
        ));
        // Resolved to the avatar (no stream yet, lookup failed, public page):
        // the later link's stream frame gets its card.
        assert!(!link_keeps_picture_claim(
            &key,
            Some(Some(
                "https://static-cdn.jtvnw.net/jtv_user_pictures/abc-profile_image-300x300.png"
            ))
        ));
        // Failed or imageless card.
        assert!(!link_keeps_picture_claim(&key, Some(None)));
        // Another channel's preview is not this channel's picture.
        assert!(!link_keeps_picture_claim(
            &key,
            Some(Some(
                "https://static-cdn.jtvnw.net/previews-ttv/live_user_otherchannel-640x360.jpg"
            ))
        ));
    }

    #[test]
    fn other_links_keep_their_claim_while_their_card_has_an_image() {
        let page = card_picture_key("https://example.com/article");
        assert!(link_keeps_picture_claim(&page, None));
        assert!(link_keeps_picture_claim(
            &page,
            Some(Some("https://example.com/og.jpg"))
        ));
        assert!(!link_keeps_picture_claim(&page, Some(None)));
        let video = card_picture_key("https://youtu.be/dQw4w9WgXcQ");
        assert!(link_keeps_picture_claim(
            &video,
            Some(Some("https://img.youtube.com/vi/dQw4w9WgXcQ/mqdefault.jpg"))
        ));
    }

    #[test]
    fn youtube_videos_and_their_thumbnails_share_a_key() {
        let video = card_picture_key("https://www.youtube.com/watch?v=dQw4w9WgXcQ");
        assert_eq!(card_picture_key("https://youtu.be/dQw4w9WgXcQ"), video);
        assert_eq!(
            card_picture_key("https://i.ytimg.com/vi/dQw4w9WgXcQ/maxresdefault.jpg"),
            video
        );
        assert_eq!(
            card_picture_key("https://img.youtube.com/vi/dQw4w9WgXcQ/mqdefault.jpg"),
            video
        );
        assert_ne!(
            card_picture_key("https://i.ytimg.com/vi/aaaaaaaaaaa/hqdefault.jpg"),
            video
        );
    }

    #[test]
    fn wikimedia_thumbnail_maps_to_the_original_upload() {
        assert_eq!(
            full_size_image_candidates(
                "https://thumb.wikimedia.org/wikipedia/commons/thumb/1/16/Apollo_11_Launch_-_GPN-2000-000630.jpg/960px-Apollo_11_Launch_-_GPN-2000-000630.jpg?utm_source=en.wikipedia.org"
            ),
            vec![
                "https://upload.wikimedia.org/wikipedia/commons/1/16/Apollo_11_Launch_-_GPN-2000-000630.jpg"
                    .to_string()
            ]
        );
        assert_eq!(
            full_size_image_candidates(
                "https://upload.wikimedia.org/wikipedia/en/thumb/a/ab/Some_file.PNG/320px-Some_file.PNG"
            ),
            vec!["https://upload.wikimedia.org/wikipedia/en/a/ab/Some_file.PNG".to_string()]
        );
    }

    #[test]
    fn wikimedia_keeps_rasterized_thumbnails_of_undecodable_originals() {
        for source in [
            "https://thumb.wikimedia.org/wikipedia/en/thumb/b/ba/Logo.svg/960px-Logo.svg.png",
            "https://upload.wikimedia.org/wikipedia/commons/thumb/1/1a/Scan.tif/lossy-page1-1200px-Scan.tif.jpg",
            "https://upload.wikimedia.org/wikipedia/commons/thumb/1/2b/Bad_shard.jpg/1200px-Bad_shard.jpg",
        ] {
            assert!(full_size_image_candidates(source).is_empty(), "{source}");
        }
        assert!(
            full_size_image_candidates(
                "https://upload.wikimedia.org/wikipedia/commons/7/79/Original.jpg"
            )
            .is_empty()
        );
    }

    #[test]
    fn spotify_cover_and_artist_art_try_larger_renditions() {
        assert_eq!(
            full_size_image_candidates(
                "https://image-cdn-fa.spotifycdn.com/image/ab67616d00001e02baf89eb11ec7c657805d2da0"
            ),
            vec![
                "https://image-cdn-fa.spotifycdn.com/image/ab67616d000082c1baf89eb11ec7c657805d2da0"
                    .to_string(),
                "https://image-cdn-fa.spotifycdn.com/image/ab67616d0000b273baf89eb11ec7c657805d2da0"
                    .to_string(),
            ]
        );
        assert_eq!(
            full_size_image_candidates(
                "https://i.scdn.co/image/ab676161000051742cb57b092374b2f975ee9adc"
            ),
            vec!["https://i.scdn.co/image/ab6761610000e5eb2cb57b092374b2f975ee9adc".to_string()]
        );
        // Playlist mosaics and unknown prefixes have no known larger form.
        assert!(
            full_size_image_candidates(
                "https://i.scdn.co/image/ab67706f00000002ffe80c69d2e795ea01e63447"
            )
            .is_empty()
        );
    }

    #[test]
    fn twitter_media_asks_for_the_original() {
        assert_eq!(
            full_size_image_candidates("https://pbs.twimg.com/media/AbCdEf?format=jpg&name=small"),
            vec!["https://pbs.twimg.com/media/AbCdEf?format=jpg&name=orig".to_string()]
        );
        assert!(
            full_size_image_candidates("https://pbs.twimg.com/media/AbCdEf?format=jpg&name=orig")
                .is_empty()
        );
    }

    #[test]
    fn discord_media_proxy_drops_only_the_resize_parameters() {
        assert_eq!(
            full_size_image_candidates(
                "https://media.discordapp.net/attachments/1/2/shot.png?ex=aa&is=bb&hm=cc&=&format=webp&width=400&height=225"
            ),
            vec![
                "https://media.discordapp.net/attachments/1/2/shot.png?ex=aa&is=bb&hm=cc&=&format=webp"
                    .to_string()
            ]
        );
        assert!(
            full_size_image_candidates(
                "https://media.discordapp.net/attachments/1/2/shot.png?ex=aa&is=bb&hm=cc"
            )
            .is_empty()
        );
    }

    #[test]
    fn twitch_live_previews_use_the_1080p_rendition() {
        let expected = vec![
            "https://static-cdn.jtvnw.net/previews-ttv/live_user_somechannel-1920x1080.jpg"
                .to_string(),
        ];
        assert_eq!(
            full_size_image_candidates(
                "https://static-cdn.jtvnw.net/previews-ttv/live_user_somechannel-640x360.jpg?t=123"
            ),
            expected
        );
        assert_eq!(
            full_size_image_candidates(
                "https://static-cdn.jtvnw.net/previews-ttv/live_user_somechannel-{width}x{height}.jpg"
            ),
            expected
        );
        assert!(
            full_size_image_candidates(
                "https://static-cdn.jtvnw.net/previews-ttv/live_user_somechannel-1920x1080.jpg"
            )
            .is_empty()
        );
    }

    #[test]
    fn youtube_thumbnails_try_larger_renditions_best_first() {
        assert_eq!(
            full_size_image_candidates("https://img.youtube.com/vi/dQw4w9WgXcQ/mqdefault.jpg"),
            vec![
                "https://i.ytimg.com/vi/dQw4w9WgXcQ/maxresdefault.jpg".to_string(),
                "https://i.ytimg.com/vi/dQw4w9WgXcQ/sddefault.jpg".to_string(),
                "https://i.ytimg.com/vi/dQw4w9WgXcQ/hqdefault.jpg".to_string(),
            ]
        );
        assert_eq!(
            full_size_image_candidates("https://i.ytimg.com/vi/dQw4w9WgXcQ/sddefault.jpg"),
            vec!["https://i.ytimg.com/vi/dQw4w9WgXcQ/maxresdefault.jpg".to_string()]
        );
        // Only a real 11-character video id is copied into a candidate URL.
        for source in [
            "https://i.ytimg.com/vi/short/mqdefault.jpg",
            "https://i.ytimg.com/vi/dQw4w9WgXcQ%2F..%2Fx/mqdefault.jpg",
        ] {
            assert!(full_size_image_candidates(source).is_empty(), "{source}");
        }
    }

    #[test]
    fn unknown_hosts_and_non_https_sources_are_left_alone() {
        for source in [
            "https://example.com/thumb/a/ab/x.jpg/100px-x.jpg",
            "http://upload.wikimedia.org/wikipedia/commons/thumb/a/ab/x.jpg/100px-x.jpg",
            "https://cdn.7tv.app/emote/01GAZ199Z8000FEWHS6AT5QZV0/4x.webp",
            "not a url",
        ] {
            assert!(full_size_image_candidates(source).is_empty(), "{source}");
        }
    }
}
