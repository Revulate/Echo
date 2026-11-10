//! Badge URL and metadata caches, and badge naming.

use super::*;

/// Entries each badge cache keeps before it resets.
pub(super) const BADGE_CACHE_CAPACITY: usize = 1000;

/// Resolved badge URLs keyed by a hash of `(badge_key, channel_id)`, so a
/// lookup allocates nothing.
pub(super) static BADGE_URL_CACHE: LazyLock<ClearOnFullMap<u64, Option<String>>> =
    LazyLock::new(|| ClearOnFullMap::new(500, BADGE_CACHE_CAPACITY));

/// Badge names, descriptions and tooltip metadata keyed by a hash of the badge key.
pub(super) static BADGE_INFO_CACHE: LazyLock<ClearOnFullMap<u64, std::sync::Arc<BadgeRenderInfo>>> =
    LazyLock::new(|| ClearOnFullMap::new(500, BADGE_CACHE_CAPACITY));

pub(super) struct BadgeRenderInfo {
    pub(super) name: SharedString,
    pub(super) description: Option<SharedString>,
    pub(super) tooltip_meta: std::sync::Arc<BadgeTooltipMeta>,
    pub(super) background: Option<Hsla>,
}

impl BadgeRenderInfo {
    pub(super) fn image_type(&self) -> ImageType {
        ImageType::Badge {
            name: self.name.clone(),
            description: self.description.clone(),
            tooltip_meta: Some(self.tooltip_meta.clone()),
            background: self.background,
        }
    }
}

/// Backing color for FFZ custom moderator badges, whose artwork is drawn on
/// a transparent background.
pub(super) const FFZ_MOD_BADGE_BACKGROUND: u32 = 0x34AE0A;

/// Hash key for a per-channel badge lookup.
#[inline]
pub(super) fn badge_cache_key(badge_key: &str, channel_id: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = rustc_hash::FxHasher::default();
    badge_key.hash(&mut hasher);
    channel_id.hash(&mut hasher);
    hasher.finish()
}

/// Drop resolved badge URLs and names so a changed badge set is picked up.
pub fn clear_badge_render_caches() {
    BADGE_URL_CACHE.clear();
    BADGE_INFO_CACHE.clear();
}

/// Resolve a badge URL through the per-channel cache.
pub(super) fn get_cached_badge_url(badge_key: &str, channel_id: &str) -> Option<String> {
    if backend::has_ffz_authority_badge(badge_key, channel_id) {
        return backend::get_badge_url(badge_key, channel_id);
    }
    let cache_key = badge_cache_key(badge_key, channel_id);

    if let Some(cached) = BADGE_URL_CACHE.get(&cache_key) {
        return cached;
    }

    let url = backend::get_badge_url(badge_key, channel_id);
    if let Some(ref resolved) = url {
        BADGE_URL_CACHE.insert(cache_key, Some(resolved.clone()));
    }
    url
}

#[inline]
pub(super) fn badge_info_cache_key(badge_key: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = rustc_hash::FxHasher::default();
    badge_key.hash(&mut hasher);
    hasher.finish()
}

pub(super) fn get_cached_badge_info(
    badge_key: &str,
    channel_id: &str,
) -> std::sync::Arc<BadgeRenderInfo> {
    if backend::has_ffz_authority_badge(badge_key, channel_id) {
        return std::sync::Arc::new(parse_badge_info(badge_key, channel_id));
    }

    let cache_key = badge_info_cache_key(badge_key);
    if let Some(cached) = BADGE_INFO_CACHE.get(&cache_key) {
        return cached;
    }

    let info = std::sync::Arc::new(parse_badge_info(badge_key, channel_id));
    BADGE_INFO_CACHE.insert(cache_key, info.clone());
    info
}

/// Parse badge key to extract name and description
pub(super) fn parse_badge_info(badge_key: &str, channel_login: &str) -> BadgeRenderInfo {
    let (badge_type_key, version_raw) = badge_key
        .split_once('/')
        .or_else(|| badge_key.split_once(':'))
        .map(|(name, version)| (name, Some(version.to_string())))
        .unwrap_or((badge_key, None));
    let external_provider = settings::external_badge_provider_for_key(badge_key);
    let external_name_is_in_version = matches!(
        external_provider,
        Some(
            settings::ExternalBadgeProvider::Ffz
                | settings::ExternalBadgeProvider::FfzBot
                | settings::ExternalBadgeProvider::Bttv
                | settings::ExternalBadgeProvider::BttvPro
        )
    );
    let display_type_key = if external_name_is_in_version {
        version_raw.as_deref().unwrap_or(badge_type_key)
    } else {
        badge_type_key
    };
    let name = echo_core::badge_key::badge_display_name(display_type_key);

    let (description, tier_label) = match (badge_type_key, version_raw.as_deref()) {
        ("subscriber", Some(version)) => {
            let parsed = version.parse::<u32>().ok();
            let tier = parsed.and_then(twitch_subscriber_tier_from_badge_version);
            let desc = match parsed {
                Some(v @ 1000) | Some(v @ 2000) | Some(v @ 3000) => {
                    Some(format!("Subscriber badge ({})", v))
                }
                Some(months) => Some(format!("{} month subscriber", months)),
                None => None,
            };
            (desc, tier)
        }
        ("founder", Some(version)) => {
            let parsed = version.parse::<u32>().ok();
            let tier = parsed.and_then(twitch_subscriber_tier_from_badge_version);
            let desc = parsed.map(|v| format!("Founder badge ({})", v));
            (desc, tier)
        }
        ("bits", Some(bits)) => (
            bits.parse::<u32>().ok().map(|b| format!("{} bits", b)),
            None,
        ),
        ("sub-gifter", Some(gifts)) => (
            gifts.parse::<u32>().ok().map(|g| format!("{} gifts", g)),
            None,
        ),
        ("follower", Some(version)) => (Some(format!("Follower badge ({})", version)), None),
        ("bttv-pro", Some("subscriber")) | ("betterttv-pro", Some("subscriber")) => {
            (Some("BetterTTV Pro Subscriber".to_string()), None)
        }
        _ => (None, None),
    };

    let is_ffz_authority_badge = backend::is_using_ffz_authority_badge(badge_key, channel_login);
    let source_label = if is_ffz_authority_badge {
        "FFZ Channel Badge"
    } else {
        external_provider
            .map(|provider| provider.source_label())
            .unwrap_or("Twitch Badge")
    };

    let tooltip_meta = BadgeTooltipMeta {
        source_label: source_label.into(),
        type_label: echo_core::badge_key::badge_display_name(display_type_key).into(),
        tier_label,
        is_animated: false,
    };

    BadgeRenderInfo {
        name: name.into(),
        description: description.map(Into::into),
        tooltip_meta: std::sync::Arc::new(tooltip_meta),
        background: (is_ffz_authority_badge
            && crate::backend::badges::is_moderator_badge_key(badge_key))
        .then(|| rgb(FFZ_MOD_BADGE_BACKGROUND).into()),
    }
}

pub(super) fn twitch_subscriber_tier_from_badge_version(version: u32) -> Option<SharedString> {
    match version {
        1000 => Some("Tier 1".into()),
        2000 => Some("Tier 2".into()),
        3000 => Some("Tier 3".into()),
        _ => None,
    }
}

pub(super) fn humanize_compound_label(raw: &str) -> String {
    raw.split(['_', '-'])
        .filter(|s| !s.is_empty())
        .map(|part| {
            let mut chars = part.chars();
            match chars.next() {
                Some(c) => {
                    let mut out = c.to_uppercase().collect::<String>();
                    out.push_str(chars.as_str());
                    out
                }
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

pub(super) fn humanize_channel_display(channel: &str) -> String {
    let mut chars = channel.chars();
    match chars.next() {
        Some(first) => {
            let mut out = first.to_uppercase().collect::<String>();
            out.push_str(chars.as_str());
            out
        }
        None => String::new(),
    }
}
