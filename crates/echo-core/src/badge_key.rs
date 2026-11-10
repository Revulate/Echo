//! Badge-key matching shared by message search, badge rendering and filters.
//!
//! A badge key is `name/version` or `name:version`. Twitch spells the lead
//! moderator badge both `lead_moderator` and `lead-moderator`, so the two
//! spellings compare equal everywhere a badge name is matched.

/// The badge name of a `name/version` or `name:version` key; a bare name is
/// returned unchanged.
pub fn badge_key_name(badge_key: &str) -> &str {
    badge_key
        .split_once('/')
        .or_else(|| badge_key.split_once(':'))
        .map(|(name, _)| name)
        .unwrap_or(badge_key)
}

/// Case-insensitive badge-name comparison that treats both lead-moderator
/// spellings as the same badge.
pub fn badge_name_matches(name: &str, expected: &str) -> bool {
    name.eq_ignore_ascii_case(expected)
        || (is_lead_moderator_badge_name(name) && is_lead_moderator_badge_name(expected))
}

/// Whether a stored badge key matches `expected`, given either as a full key
/// or as a bare badge name.
pub fn badge_key_matches(badge_key: &str, expected: &str) -> bool {
    badge_name_matches(badge_key, expected)
        || badge_name_matches(badge_key_name(badge_key), expected)
}

pub fn is_lead_moderator_badge_name(name: &str) -> bool {
    name.eq_ignore_ascii_case("lead_moderator") || name.eq_ignore_ascii_case("lead-moderator")
}

/// Whether the key is a moderator or lead-moderator badge.
pub fn is_moderator_badge_key(badge_key: &str) -> bool {
    let name = badge_key_name(badge_key);
    name.eq_ignore_ascii_case("moderator") || is_lead_moderator_badge_name(name)
}

/// The name shown for a badge set, such as `VIP` for `vip` and `Turbo` for
/// `premium`. Other names are title-cased word by word.
pub fn badge_display_name(name: &str) -> String {
    let known = match name {
        "subscriber" => "Subscriber",
        "moderator" => "Moderator",
        "lead_moderator" | "lead-moderator" => "Lead Moderator",
        "broadcaster" => "Broadcaster",
        "vip" => "VIP",
        "bits" => "Bits",
        "sub-gifter" => "Sub Gifter",
        "premium" => "Turbo",
        "partner" => "Partner",
        "founder" => "Founder",
        "artist-badge" => "Artist",
        _ => "",
    };
    if !known.is_empty() {
        return known.to_string();
    }
    let mut out = String::with_capacity(name.len());
    for word in name.split(['-', '_']).filter(|word| !word.is_empty()) {
        if !out.is_empty() {
            out.push(' ');
        }
        let mut chars = word.chars();
        if let Some(first) = chars.next() {
            out.extend(first.to_uppercase());
            out.push_str(chars.as_str());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn moderator_badges_match_only_moderator_and_lead_aliases() {
        assert!(is_moderator_badge_key("moderator/1"));
        assert!(is_moderator_badge_key("LEAD-MODERATOR:1"));
        assert!(badge_key_matches("subscriber:12", "subscriber:12"));
        assert!(badge_key_matches("lead_moderator/1", "LEAD-MODERATOR"));
        for badge in ["broadcaster/1", "staff/1", "admin/1", "global-mod/1"] {
            assert!(!is_moderator_badge_key(badge), "{badge}");
        }
    }
}
