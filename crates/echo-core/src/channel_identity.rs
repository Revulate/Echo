//! GPUI-free identity policy for Echo's virtual channel leaves.
//!
//! A layout leaf can hold an ordinary Twitch login, the stable Watching
//! sentinel, or the Whispers channel. The two sentinel predicates are pure, so
//! they live here directly. Resolving the Watching sentinel to the login the
//! browser currently reports is a runtime lookup, so the root installs it as a
//! port; without an installed port a Watching leaf simply resolves to nothing,
//! which is also what it does before the browser reports a selection.

use std::{borrow::Cow, sync::OnceLock};

/// Stable persisted layout value for the browser Watching leaf.
pub const WATCHING_CHANNEL: &str = "__echo_watching__";

/// Stable channel name for the Whispers leaf.
pub const WHISPERS_CHANNEL: &str = "whispers";

static CURRENT_WATCHING_CHANNEL: OnceLock<fn() -> Option<String>> = OnceLock::new();

/// Install the runtime lookup for the browser-selected Watching login.
pub fn install_current_watching_channel(callback: fn() -> Option<String>) {
    let _ = CURRENT_WATCHING_CHANNEL.set(callback);
}

/// Whether this layout value is the Watching sentinel.
pub fn is_watching_channel(channel: &str) -> bool {
    channel == WATCHING_CHANNEL
}

/// Whether this layout value addresses the Whispers channel.
pub fn is_whispers_channel(channel: &str) -> bool {
    channel
        .trim()
        .trim_start_matches('#')
        .eq_ignore_ascii_case(WHISPERS_CHANNEL)
}

/// The normalized Twitch login currently selected for the Watching leaf.
pub fn current_watching_channel() -> Option<String> {
    CURRENT_WATCHING_CHANNEL
        .get()
        .and_then(|callback| callback())
}

/// Resolve a layout value to the concrete channel it addresses.
pub fn resolve_layout_channel(channel: &str) -> Option<String> {
    if is_watching_channel(channel) {
        current_watching_channel()
    } else {
        Some(channel.to_string())
    }
}

/// The lowercase, `#`-free key Echo stores and compares a channel under.
///
/// Twitch logins are ASCII, so ASCII lowercasing is the whole normalization.
/// Surrounding whitespace is kept: callers whose input may carry it (user
/// input, imported files) pass `channel.trim()`.
pub fn normalize_channel_key(channel: &str) -> String {
    channel_key_cow(channel).into_owned()
}

/// [`normalize_channel_key`] without allocating when `channel` is already a key.
#[inline]
pub fn channel_key_cow(channel: &str) -> Cow<'_, str> {
    let trimmed = channel.trim_start_matches('#');
    if trimmed.bytes().all(|byte| !byte.is_ascii_uppercase()) {
        Cow::Borrowed(trimmed)
    } else {
        Cow::Owned(trimmed.to_ascii_lowercase())
    }
}

#[cfg(test)]
mod tests {
    use super::{channel_key_cow, normalize_channel_key};
    use std::borrow::Cow;

    const INPUTS: &[&str] = &[
        "",
        "#",
        "forsen",
        "#Forsen",
        "##XQC",
        "  #Echo_Dev ",
        "#ÉCHO",
        "ﬁx",
        "#İstanbul",
    ];

    #[test]
    fn key_strips_every_leading_hash_and_lowercases_ascii() {
        assert_eq!(normalize_channel_key("#Forsen"), "forsen");
        assert_eq!(normalize_channel_key("##XQC"), "xqc");
        assert_eq!(normalize_channel_key("a#B"), "a#b");
        assert_eq!(normalize_channel_key(" #Echo "), " #echo ");
        assert_eq!(normalize_channel_key("#ÉCHO"), "Écho");
    }

    #[test]
    fn cow_borrows_input_that_is_already_a_key() {
        assert!(matches!(
            channel_key_cow("#forsen"),
            Cow::Borrowed("forsen")
        ));
        assert!(matches!(channel_key_cow("Forsen"), Cow::Owned(_)));
        for input in INPUTS {
            assert_eq!(channel_key_cow(input), normalize_channel_key(input));
        }
    }

    /// `channel.trim_start_matches('#').to_ascii_lowercase()` is this function.
    #[test]
    fn untrimmed_ascii_variant_is_the_key() {
        for input in INPUTS {
            assert_eq!(
                input.trim_start_matches('#').to_ascii_lowercase(),
                normalize_channel_key(input)
            );
        }
    }

    /// `channel.trim().trim_start_matches('#').to_ascii_lowercase()` is the key
    /// of the trimmed input.
    #[test]
    fn trimmed_ascii_variant_is_the_key_of_the_trimmed_input() {
        for input in INPUTS {
            assert_eq!(
                input.trim().trim_start_matches('#').to_ascii_lowercase(),
                normalize_channel_key(input.trim())
            );
        }
    }

    /// The Unicode-lowercasing variants (`.to_lowercase()`) agree on ASCII
    /// logins but not beyond them, so they are not interchangeable with the key.
    #[test]
    fn unicode_lowercase_variants_differ_outside_ascii() {
        for input in ["forsen", "#Forsen", "##XQC", "#Echo_Dev"] {
            assert_eq!(
                input.trim_start_matches('#').to_lowercase(),
                normalize_channel_key(input)
            );
        }
        assert_ne!(
            "#ÉCHO".trim_start_matches('#').to_lowercase(),
            normalize_channel_key("#ÉCHO")
        );
        assert_ne!(
            "#İstanbul".trim().trim_start_matches('#').to_lowercase(),
            normalize_channel_key("#İstanbul".trim())
        );
    }
}
