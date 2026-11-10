/// Echo's Twitch Application Client ID (EchoChambers - Public client)
pub const CLIENT_ID: &str = "vl8xi4z5tcfvr8qsdmzrsipxe48sbn";

/// Optional Echo API proxy for Twitch data that needs no user login, as
/// `(base URL, key)`. The key is sent as `X-Echo-Key` for abuse prevention, not
/// security. Set at build time through `ECHO_API_PROXY_URL` and
/// `ECHO_API_PROXY_KEY` (see build.rs); `None` when the build has no proxy.
/// Endpoints:
///   GET /badges/global
///   GET /badges/channel/{channel_id}
///   GET /users/{login}
///   GET /streams/{login}
pub const API_PROXY: Option<(&str, &str)> = match (
    option_env!("ECHO_API_PROXY_URL"),
    option_env!("ECHO_API_PROXY_KEY"),
) {
    (Some(url), Some(key)) => Some((url, key)),
    _ => None,
};

/// Error returned by lookups that need [`API_PROXY`] in a build without one.
pub const API_PROXY_NOT_CONFIGURED: &str = "API proxy not configured";

/// Helix API endpoints
pub const HELIX_BADGES_GLOBAL_ENDPOINT: &str = "https://api.twitch.tv/helix/chat/badges/global";
pub const HELIX_WHISPERS_ENDPOINT: &str = "https://api.twitch.tv/helix/whispers";
pub const TWITCH_WHISPER_MANAGE_SCOPE: &str = "user:manage:whispers";
