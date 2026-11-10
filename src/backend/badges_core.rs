//! Pure badge-key helpers shared by runtime badge rendering and offline checks.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExternalBadgeProvider {
    Ffz,
    Bttv,
    BttvPro,
    FfzBot,
}

impl ExternalBadgeProvider {
    pub fn source_label(self) -> &'static str {
        match self {
            Self::Ffz => "FFZ Badge",
            Self::Bttv => "BTTV Badge",
            Self::BttvPro => "BTTV Pro Badge",
            Self::FfzBot => "FFZ Bot Badge",
        }
    }
}

pub const NO_AUDIO_BADGE_KEY: &str = "no_audio:1";
pub const NO_AUDIO_BADGE_URL: &str =
    "https://static-cdn.jtvnw.net/badges/v1/aef2cd08-f29b-45a1-8c12-d44571c0ebf5/3";
pub const NO_VIDEO_BADGE_KEY: &str = "no_video:1";
pub const NO_VIDEO_BADGE_URL: &str =
    "https://static-cdn.jtvnw.net/badges/v1/199a0dba-58f3-494e-a7fc-1fa0a1001fb8/3";
pub const BTTV_PRO_BADGE_KEY: &str = "bttv-pro:subscriber";

pub fn badge_key_name(badge_key: &str) -> &str {
    badge_key
        .split_once('/')
        .or_else(|| badge_key.split_once(':'))
        .map(|(name, _)| name)
        .unwrap_or(badge_key)
}

pub fn badge_name_matches(name: &str, expected: &str) -> bool {
    name.eq_ignore_ascii_case(expected)
        || (is_lead_moderator_badge_name(name) && is_lead_moderator_badge_name(expected))
}

pub fn badge_key_matches(badge_key: &str, expected: &str) -> bool {
    badge_name_matches(badge_key, expected)
        || badge_name_matches(badge_key_name(badge_key), expected)
}

pub fn is_lead_moderator_badge_name(name: &str) -> bool {
    name.eq_ignore_ascii_case("lead_moderator") || name.eq_ignore_ascii_case("lead-moderator")
}

pub fn is_moderator_badge_key(badge_key: &str) -> bool {
    let name = badge_key_name(badge_key);
    badge_name_matches(name, "moderator") || is_lead_moderator_badge_name(name)
}

pub fn external_badge_provider_for_key(badge_key: &str) -> Option<ExternalBadgeProvider> {
    const BTTV_PRO_PREFIXES: &[&str] = &[
        "bttv-pro",
        "bttv_pro",
        "bttvpro",
        "betterttv-pro",
        "betterttv_pro",
        "betterttvpro",
    ];
    const FFZ_BOT_PREFIXES: &[&str] = &[
        "ffz-bot",
        "ffz_bot",
        "ffzbot",
        "frankerfacez-bot",
        "frankerfacez_bot",
        "frankerfacezbot",
    ];
    const FFZ_PREFIXES: &[&str] = &["ffz", "frankerfacez"];
    const BTTV_PREFIXES: &[&str] = &["bttv", "betterttv"];

    if has_badge_provider_prefix(badge_key, BTTV_PRO_PREFIXES) {
        Some(ExternalBadgeProvider::BttvPro)
    } else if has_badge_provider_prefix(badge_key, FFZ_BOT_PREFIXES) {
        Some(ExternalBadgeProvider::FfzBot)
    } else if has_badge_provider_prefix(badge_key, FFZ_PREFIXES) {
        Some(ExternalBadgeProvider::Ffz)
    } else if has_badge_provider_prefix(badge_key, BTTV_PREFIXES) {
        Some(ExternalBadgeProvider::Bttv)
    } else {
        None
    }
}

fn has_badge_provider_prefix(badge_key: &str, prefixes: &[&str]) -> bool {
    prefixes.iter().any(|prefix| {
        let Some(head) = badge_key.get(..prefix.len()) else {
            return false;
        };
        if !head.eq_ignore_ascii_case(prefix) {
            return false;
        }

        let Some(rest) = badge_key.get(prefix.len()..) else {
            return false;
        };
        matches!(
            rest.as_bytes().first(),
            None | Some(b'/' | b':' | b'|' | b'.' | b'-' | b'_')
        )
    })
}

pub fn embedded_twitch_badge_fallback_url(badge_key: &str) -> Option<&'static str> {
    match normalize_badge_cache_key(badge_key).as_str() {
        NO_AUDIO_BADGE_KEY => Some(NO_AUDIO_BADGE_URL),
        NO_VIDEO_BADGE_KEY => Some(NO_VIDEO_BADGE_URL),
        _ => None,
    }
}

pub fn ffz_badge_cache_key(name: &str) -> String {
    let normalized = normalize_provider_key_part(name);
    if normalized == "bot" {
        format!("ffz-bot:{normalized}")
    } else {
        format!("ffz:{normalized}")
    }
}

pub fn bttv_badge_cache_key(badge_type: u64, description: &str) -> String {
    let normalized = match badge_type {
        1 => "developer".to_string(),
        2 => "support-volunteer".to_string(),
        3 => "emote-approver".to_string(),
        4 => "translator".to_string(),
        _ => normalize_provider_key_part(description),
    };
    format!("bttv:{normalized}")
}

pub fn bttv_pro_badge_cache_key() -> &'static str {
    BTTV_PRO_BADGE_KEY
}

pub fn normalize_badge_cache_key(badge_key: &str) -> String {
    if badge_key.contains('/') && !badge_key.contains(':') {
        badge_key.replacen('/', ":", 1)
    } else {
        badge_key.to_string()
    }
}

fn normalize_provider_key_part(value: &str) -> String {
    value.trim().to_ascii_lowercase().replace(' ', "-")
}
