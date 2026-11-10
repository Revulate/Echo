use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// One emote from any provider, as cached, persisted and rendered.
///
/// `url` is the default display copy; `url_1x`/`url_2x`/`url_4x` are the
/// provider's sized copies when known. `width`/`height` are the logical 1x
/// size, `None` when the provider did not report it.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Emote {
    pub id: Arc<str>,
    pub name: Arc<str>,
    pub url: Arc<str>,
    pub provider: EmoteProvider,
    /// Where the emote can be used, in its persisted encoding; read it through
    /// [`Emote::scope`].
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

/// The service an emote comes from. Variant names are part of the persisted
/// cache format.
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

/// [`Emote::channel`] prefix of a Twitch subscriber emote, followed by the
/// owning channel's login.
pub const SUBSCRIPTION_CHANNEL_PREFIX: &str = "sub:";
/// [`Emote::channel`] of an emote from the signed-in user's 7TV personal set.
pub const PERSONAL_SEVENTV_CHANNEL: &str = "personal:7tv";

/// Where an emote can be used, decoded from [`Emote::channel`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EmoteScope<'a> {
    /// Usable everywhere.
    Global,
    /// A channel's own set, keyed by channel login, or another provider
    /// grouping such as Twitch's `twitch:prime`.
    Channel(&'a str),
    /// A Twitch subscriber emote of the given channel login.
    Subscription(&'a str),
    /// The signed-in user's 7TV personal set.
    Personal,
}

/// Decode an [`Emote::channel`] value.
pub fn emote_scope(channel: Option<&str>) -> EmoteScope<'_> {
    match channel {
        None => EmoteScope::Global,
        Some(PERSONAL_SEVENTV_CHANNEL) => EmoteScope::Personal,
        Some(channel) => match channel.strip_prefix(SUBSCRIPTION_CHANNEL_PREFIX) {
            Some(owner) => EmoteScope::Subscription(owner),
            None => EmoteScope::Channel(channel),
        },
    }
}

impl Emote {
    /// Where this emote can be used.
    pub fn scope(&self) -> EmoteScope<'_> {
        emote_scope(self.channel.as_deref())
    }
}

pub fn emote_url_shared(emote: &Emote, selected: &str) -> Arc<str> {
    shared_variant_url(
        &emote.url,
        [&emote.url_1x, &emote.url_2x, &emote.url_4x],
        selected,
    )
}

/// The emote's own `Arc` for `selected` when it is one of its URLs, so callers
/// share it instead of allocating a copy.
pub(crate) fn shared_variant_url(
    url: &Arc<str>,
    variants: [&Option<Arc<str>>; 3],
    selected: &str,
) -> Arc<str> {
    if url.as_ref() == selected {
        return Arc::clone(url);
    }
    for candidate in variants {
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

/// Which of an emote's copies a URL is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceTier {
    /// The `url_1x` copy.
    One,
    /// The `url_2x` copy.
    Two,
    /// The `url_4x` slot: the provider's largest copy (Twitch 3.0, BTTV 3x,
    /// FFZ 4, 7TV 4x, or 7TV 3x when that is its largest file).
    Largest,
    /// The default display URL, or any URL that is none of the sized copies.
    Default,
}

impl SourceTier {
    pub fn of(
        url_1x: Option<&str>,
        url_2x: Option<&str>,
        url_4x: Option<&str>,
        selected: &str,
    ) -> Self {
        if url_1x == Some(selected) {
            Self::One
        } else if url_2x == Some(selected) {
            Self::Two
        } else if url_4x == Some(selected) {
            Self::Largest
        } else {
            Self::Default
        }
    }

    /// The provider's own name for this copy. It keys persisted media
    /// identities, so the spellings are part of the cache format.
    pub fn label(self, provider: EmoteProvider, selected: &str) -> &'static str {
        match self {
            Self::One => match provider {
                EmoteProvider::Twitch => "1.0",
                EmoteProvider::FFZ => "1",
                _ => "1x",
            },
            Self::Two => match provider {
                EmoteProvider::Twitch => "2.0",
                EmoteProvider::FFZ => "2",
                _ => "2x",
            },
            Self::Largest => match provider {
                EmoteProvider::Twitch => "3.0",
                EmoteProvider::BTTV => "3x",
                EmoteProvider::FFZ => "4",
                EmoteProvider::SevenTV if is_seventv_three_x_file(selected) => "3x",
                _ => "4x",
            },
            Self::Default => "default",
        }
    }

    /// Physical pixels per logical pixel of this copy.
    pub fn density(self, provider: EmoteProvider, emote_id: &str, selected: &str) -> f32 {
        match self {
            Self::One | Self::Default => 1.0,
            Self::Two => 2.0,
            Self::Largest => match provider {
                EmoteProvider::Twitch => crate::twitch_high_tier_density(emote_id),
                EmoteProvider::SevenTV if is_seventv_three_x_file(selected) => 3.0,
                _ => 4.0,
            },
        }
    }
}

fn is_seventv_three_x_file(url: &str) -> bool {
    url.rsplit('/')
        .next()
        .is_some_and(|file| file.starts_with("3x.") || file.starts_with("3x_"))
}

pub fn provider_variant_scale(
    provider: EmoteProvider,
    url_1x: Option<&str>,
    url_2x: Option<&str>,
    url_4x: Option<&str>,
    selected: &str,
) -> &'static str {
    SourceTier::of(url_1x, url_2x, url_4x, selected).label(provider, selected)
}

pub fn emote_source_density(emote: &Emote, url: &str) -> f32 {
    SourceTier::of(
        emote.url_1x.as_deref(),
        emote.url_2x.as_deref(),
        emote.url_4x.as_deref(),
        url,
    )
    .density(emote.provider, &emote.id, url)
}

/// Canonical 2x Twitch CDN URL for an emote id.
pub fn twitch_emote_url(id: &str) -> String {
    twitch_emote_url_with_scale(id, "2.0")
}

/// Twitch CDN URL for an emote id at an explicit scale.
pub fn twitch_emote_url_with_scale(id: &str, scale: &str) -> String {
    format!(
        "https://static-cdn.jtvnw.net/emoticons/v2/{}/default/dark/{}",
        id, scale
    )
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
