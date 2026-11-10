use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Emote {
    pub id: Arc<str>,
    pub name: Arc<str>,
    pub url: Arc<str>,
    pub provider: EmoteProvider,
    #[serde(default)]
    pub channel: Option<Arc<str>>,
    #[serde(default)]
    pub original_name: Option<Arc<str>>,
    #[serde(default)]
    pub creator_name: Option<Arc<str>>,
    #[serde(default)]
    pub twitch_owner_login: Option<Arc<str>>,
    #[serde(default)]
    pub twitch_owner_id: Option<Arc<str>>,
    #[serde(default)]
    pub twitch_emote_type: Option<Arc<str>>,
    #[serde(default)]
    pub url_1x: Option<Arc<str>>,
    #[serde(default)]
    pub url_2x: Option<Arc<str>>,
    #[serde(default)]
    pub url_4x: Option<Arc<str>>,
    #[serde(default)]
    pub zero_width: bool,
    #[serde(default)]
    pub is_animated: bool,
    #[serde(default)]
    pub width: Option<u32>,
    #[serde(default)]
    pub height: Option<u32>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[allow(clippy::upper_case_acronyms)]
pub enum EmoteProvider {
    Twitch,
    SevenTV,
    BTTV,
    FFZ,
    Emoji,
}

impl EmoteProvider {
    pub fn stable_cache_key(self) -> &'static str {
        match self {
            Self::Twitch => "twitch",
            Self::SevenTV => "7tv",
            Self::BTTV => "bttv",
            Self::FFZ => "ffz",
            Self::Emoji => "emoji",
        }
    }
}

pub fn emote_url_shared(emote: &Emote, selected: &str) -> Arc<str> {
    if emote.url.as_ref() == selected {
        return Arc::clone(&emote.url);
    }
    for candidate in [&emote.url_1x, &emote.url_2x, &emote.url_4x] {
        if let Some(url) = candidate.as_ref().filter(|url| url.as_ref() == selected) {
            return Arc::clone(url);
        }
    }
    Arc::from(selected)
}

pub fn emote_source_scale(emote: &Emote, url: &str) -> &'static str {
    provider_variant_scale(
        emote.provider,
        emote.url_1x.as_deref(),
        emote.url_2x.as_deref(),
        emote.url_4x.as_deref(),
        url,
    )
}

pub fn provider_variant_scale(
    provider: EmoteProvider,
    url_1x: Option<&str>,
    url_2x: Option<&str>,
    url_4x: Option<&str>,
    selected: &str,
) -> &'static str {
    if url_1x == Some(selected) {
        return match provider {
            EmoteProvider::Twitch => "1.0",
            EmoteProvider::FFZ => "1",
            _ => "1x",
        };
    }
    if url_2x == Some(selected) {
        return match provider {
            EmoteProvider::Twitch => "2.0",
            EmoteProvider::FFZ => "2",
            _ => "2x",
        };
    }
    if url_4x == Some(selected) {
        return match provider {
            EmoteProvider::Twitch => "3.0",
            EmoteProvider::BTTV => "3x",
            EmoteProvider::FFZ => "4",
            EmoteProvider::SevenTV
                if selected
                    .rsplit('/')
                    .next()
                    .is_some_and(|file| file.starts_with("3x.") || file.starts_with("3x_")) =>
            {
                "3x"
            }
            _ => "4x",
        };
    }
    "default"
}

pub fn emote_source_density(emote: &Emote, url: &str) -> f32 {
    match emote_source_scale(emote, url) {
        "2.0" | "2" | "2x" => 2.0,
        "3.0" => crate::twitch_high_tier_density(&emote.id),
        "3x" if emote.provider == EmoteProvider::SevenTV => 3.0,
        "3x" | "4" | "4x" => 4.0,
        _ => 1.0,
    }
}

/// Canonical 2x Twitch CDN URL for an emote id.
pub fn twitch_emote_url(id: &str) -> String {
    format!(
        "https://static-cdn.jtvnw.net/emoticons/v2/{}/default/dark/2.0",
        id
    )
}

/// Twitch CDN URL for an emote id at an explicit scale.
pub fn twitch_emote_url_with_scale(id: &str, scale: &str) -> String {
    format!(
        "https://static-cdn.jtvnw.net/emoticons/v2/{}/default/dark/{}",
        id, scale
    )
}

/// Normalize a channel name into the lowercase, '#'-free cache key.
pub fn normalize_channel_key(channel: &str) -> String {
    normalized_channel_key(channel).into_owned()
}

#[inline]
pub fn normalized_channel_key(channel: &str) -> std::borrow::Cow<'_, str> {
    let trimmed = channel.trim_start_matches('#');
    if trimmed.bytes().all(|byte| !byte.is_ascii_uppercase()) {
        std::borrow::Cow::Borrowed(trimmed)
    } else {
        std::borrow::Cow::Owned(trimmed.to_ascii_lowercase())
    }
}

/// Smaller copies of a provider emote image, largest first.
///
/// Used when the requested copy is missing, fails to decode, or is an
/// animation too large to play, so the emote still shows at a lower
/// resolution. Recognizes the 7TV, BTTV, FFZ and Twitch emote CDN URLs Echo
/// builds; anything else (paints, badges, link media) has no fallbacks.
pub fn lower_resolution_emote_urls(url: &str) -> Vec<String> {
    const SEVENTV: &[&str] = &["4x", "3x", "2x", "1x"];
    const BTTV: &[&str] = &["3x", "2x", "1x"];
    const FFZ: &[&str] = &["4", "2", "1"];
    const TWITCH: &[&str] = &["3", "2", "1"];

    let seventv = url.contains("cdn.7tv.app/emote/");
    let ladder = if seventv {
        SEVENTV
    } else if url.contains("cdn.betterttv.net/emote/") {
        BTTV
    } else if url.contains("cdn.frankerfacez.com/emote/") {
        FFZ
    } else if url.contains("static-cdn.jtvnw.net/emoticons/v2/") {
        TWITCH
    } else {
        return Vec::new();
    };
    let Some((dir, file)) = url.rsplit_once('/') else {
        return Vec::new();
    };
    // "4x.webp", "4x_static.webp", "3x", "4", "3.0": scale token, then suffix.
    let split = file
        .find(|c: char| !c.is_ascii_alphanumeric())
        .unwrap_or(file.len());
    let (scale, suffix) = file.split_at(split);
    let Some(position) = ladder.iter().position(|step| *step == scale) else {
        return Vec::new();
    };
    ladder[position + 1..]
        .iter()
        // A 7TV 4x copy skips 3x: when 4x exceeds the decode limits 3x does
        // too (vulpSaj, peepoSpace), so trying it only costs a wasted decode.
        // Emotes whose largest 7TV file is 3x still step down to 2x.
        .filter(|step| !(seventv && scale == "4x" && **step == "3x"))
        .map(|step| format!("{dir}/{step}{suffix}"))
        .collect()
}

#[cfg(test)]
mod lower_resolution_tests {
    use super::{EmoteProvider, lower_resolution_emote_urls, provider_variant_scale};

    #[test]
    fn seventv_three_x_fallback_retains_its_actual_density() {
        for url in [
            "https://cdn.7tv.app/emote/id/3x.webp",
            "https://cdn.7tv.app/emote/id/3x_static.webp",
        ] {
            assert_eq!(
                provider_variant_scale(EmoteProvider::SevenTV, None, None, Some(url), url),
                "3x"
            );
        }
    }

    #[test]
    fn seventv_steps_down_to_2x_then_1x_keeping_the_format() {
        assert_eq!(
            lower_resolution_emote_urls("https://cdn.7tv.app/emote/01ABC/4x.webp"),
            [
                "https://cdn.7tv.app/emote/01ABC/2x.webp",
                "https://cdn.7tv.app/emote/01ABC/1x.webp",
            ]
        );
        assert_eq!(
            lower_resolution_emote_urls("https://cdn.7tv.app/emote/01ABC/3x.webp"),
            [
                "https://cdn.7tv.app/emote/01ABC/2x.webp",
                "https://cdn.7tv.app/emote/01ABC/1x.webp",
            ]
        );
        assert_eq!(
            lower_resolution_emote_urls("https://cdn.7tv.app/emote/01ABC/2x_static.avif"),
            ["https://cdn.7tv.app/emote/01ABC/1x_static.avif"]
        );
    }

    #[test]
    fn bttv_ffz_and_twitch_use_their_own_scale_names() {
        assert_eq!(
            lower_resolution_emote_urls("https://cdn.betterttv.net/emote/5f1b/3x"),
            [
                "https://cdn.betterttv.net/emote/5f1b/2x",
                "https://cdn.betterttv.net/emote/5f1b/1x",
            ]
        );
        assert_eq!(
            lower_resolution_emote_urls("https://cdn.frankerfacez.com/emote/1234/animated/4"),
            [
                "https://cdn.frankerfacez.com/emote/1234/animated/2",
                "https://cdn.frankerfacez.com/emote/1234/animated/1",
            ]
        );
        assert_eq!(
            lower_resolution_emote_urls(
                "https://static-cdn.jtvnw.net/emoticons/v2/25/animated/dark/3.0"
            ),
            [
                "https://static-cdn.jtvnw.net/emoticons/v2/25/animated/dark/2.0",
                "https://static-cdn.jtvnw.net/emoticons/v2/25/animated/dark/1.0",
            ]
        );
    }

    #[test]
    fn smallest_copies_and_other_media_have_no_fallback() {
        assert!(lower_resolution_emote_urls("https://cdn.7tv.app/emote/01ABC/1x.webp").is_empty());
        assert!(
            lower_resolution_emote_urls("https://cdn.7tv.app/paint/01P/layer/01L/4x.webp")
                .is_empty()
        );
        assert!(lower_resolution_emote_urls("https://olrite.lol/a.png").is_empty());
    }
}
