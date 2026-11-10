//! Normalization of what a user types into a channel chooser.

/// twitch.tv paths whose next segment is the channel.
const CHANNEL_PATH_PREFIXES: &[&str] = &["popout/", "moderator/", "embed/", "subs/"];

/// First twitch.tv path segments that are site pages, not channels.
const NON_CHANNEL_PATHS: &[&str] = &[
    "bits",
    "directory",
    "downloads",
    "drops",
    "friends",
    "following",
    "inventory",
    "jobs",
    "login",
    "messages",
    "p",
    "prime",
    "search",
    "settings",
    "signup",
    "store",
    "subscriptions",
    "turbo",
    "videos",
    "wallet",
];

/// Why a typed channel cannot be joined.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChannelInputError {
    Invalid,
}

impl ChannelInputError {
    pub fn message(self) -> &'static str {
        match self {
            Self::Invalid => "Twitch channel names use 1-25 letters, numbers, or underscores",
        }
    }
}

/// Turn `#name`, `@name`, `name`, or a `twitch.tv/name` URL into a Twitch login.
///
/// Empty input stays empty. Echo's own layout values (the Watching sentinel and
/// the Whispers channel) pass through unchanged.
pub fn normalize_channel_input(raw: &str) -> Result<String, ChannelInputError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(String::new());
    }
    if echo_core::channel_identity::is_watching_channel(trimmed) {
        return Ok(trimmed.to_string());
    }
    if echo_core::channel_identity::is_whispers_channel(trimmed) {
        return Ok(echo_core::channel_identity::WHISPERS_CHANNEL.to_string());
    }

    let lower = trimmed.to_lowercase();
    let mut rest = lower.as_str();
    for scheme in ["https://", "http://"] {
        if let Some(stripped) = rest.strip_prefix(scheme) {
            rest = stripped;
            break;
        }
    }
    let mut from_url = false;
    for host in ["www.twitch.tv/", "m.twitch.tv/", "twitch.tv/"] {
        if let Some(stripped) = rest.strip_prefix(host) {
            rest = stripped;
            from_url = true;
            break;
        }
    }
    if let Some(channel_path) = CHANNEL_PATH_PREFIXES
        .iter()
        .find_map(|prefix| rest.strip_prefix(prefix))
    {
        rest = channel_path;
    }
    let segment = rest
        .split(['/', '?', '#'])
        .find(|part| !part.is_empty())
        .unwrap_or(rest);
    if from_url && NON_CHANNEL_PATHS.contains(&segment) {
        return Err(ChannelInputError::Invalid);
    }
    let login = segment.trim_start_matches(['#', '@']);
    let login = if login.is_empty() {
        lower.trim_start_matches(['#', '@'])
    } else {
        login
    };

    if is_valid_twitch_login(login) {
        Ok(login.to_string())
    } else {
        Err(ChannelInputError::Invalid)
    }
}

/// Twitch login charset: 1-25 of `[a-z0-9_]`. Safe as a file name or a URL
/// path segment as-is.
pub fn is_valid_twitch_login(login: &str) -> bool {
    (1..=25).contains(&login.len())
        && login
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test;

    #[test]
    fn strips_prefixes_and_urls() {
        for input in [
            "xqc",
            "#xqc",
            "@XQC",
            "  xQc ",
            "twitch.tv/xqc",
            "https://www.twitch.tv/xqc",
            "https://twitch.tv/xqc/videos?filter=all",
            "https://www.twitch.tv/popout/xqc/chat?popout=",
            "m.twitch.tv/xqc",
            "https://www.twitch.tv/moderator/xqc",
            "twitch.tv/embed/xqc/chat",
        ] {
            assert_eq!(
                normalize_channel_input(input),
                Ok("xqc".to_string()),
                "{input}"
            );
        }
    }

    #[test]
    fn rejects_names_twitch_cannot_have() {
        for input in [
            "not a channel",
            "x.q.c",
            "a".repeat(26).as_str(),
            "ümlaut",
            "#",
            "https://www.twitch.tv/videos/123456",
            "twitch.tv/directory/category/just-chatting",
            "https://www.twitch.tv/settings/profile",
        ] {
            assert_eq!(
                normalize_channel_input(input),
                Err(ChannelInputError::Invalid),
                "{input}"
            );
        }
    }

    #[test]
    fn keeps_empty_and_special_channels() {
        assert_eq!(normalize_channel_input("   "), Ok(String::new()));
        assert_eq!(
            normalize_channel_input("#Whispers"),
            Ok("whispers".to_string())
        );
        assert_eq!(
            normalize_channel_input(echo_core::channel_identity::WATCHING_CHANNEL),
            Ok(echo_core::channel_identity::WATCHING_CHANNEL.to_string())
        );
    }
}
