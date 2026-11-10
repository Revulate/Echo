use url::Url;

/// The Spotify resource families intentionally supported by link previews.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpotifyResourceKind {
    Track,
    Album,
    Playlist,
    Artist,
    Episode,
    Show,
}

/// Parse only canonical public Spotify resource pages.
pub fn parse_spotify_resource_url(value: &str) -> Option<(SpotifyResourceKind, String)> {
    let url = Url::parse(value).ok()?;
    if url.scheme() != "https"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.port().is_some()
        || !url.host_str()?.eq_ignore_ascii_case("open.spotify.com")
    {
        return None;
    }

    let mut parts = url.path_segments()?;
    let resource_kind = SpotifyResourceKind::from_path_segment(parts.next()?)?;
    let spotify_id = parts.next()?;
    if parts.next().is_some() || !is_valid_spotify_id(spotify_id) {
        return None;
    }

    Some((resource_kind, spotify_id.to_string()))
}

impl SpotifyResourceKind {
    /// Parse the canonical `open.spotify.com` path segment.
    pub fn from_path_segment(value: &str) -> Option<Self> {
        match value {
            "track" => Some(Self::Track),
            "album" => Some(Self::Album),
            "playlist" => Some(Self::Playlist),
            "artist" => Some(Self::Artist),
            "episode" => Some(Self::Episode),
            "show" => Some(Self::Show),
            _ => None,
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Track => "track",
            Self::Album => "album",
            Self::Playlist => "playlist",
            Self::Artist => "artist",
            Self::Episode => "episode",
            Self::Show => "show",
        }
    }
}

pub fn is_valid_spotify_id(value: &str) -> bool {
    value.len() == 22 && value.bytes().all(|byte| byte.is_ascii_alphanumeric())
}

/// The public Steam-family application-page source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SteamAppSource {
    Steam,
    SteamDb,
    ProtonDb,
}

impl SteamAppSource {
    pub const fn provider_label(self) -> &'static str {
        match self {
            Self::Steam => "Steam",
            Self::SteamDb => "SteamDB",
            Self::ProtonDb => "ProtonDB",
        }
    }
}

/// Recognize only canonical HTTPS public application pages.
pub fn parse_steam_app_url(value: &str) -> Option<(SteamAppSource, String)> {
    let url = Url::parse(value).ok()?;
    if url.scheme() != "https"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.port().is_some()
    {
        return None;
    }

    let source = match url.host_str()? {
        host if host.eq_ignore_ascii_case("store.steampowered.com") => SteamAppSource::Steam,
        host if host.eq_ignore_ascii_case("steamdb.info") => SteamAppSource::SteamDb,
        host if host.eq_ignore_ascii_case("protondb.com") => SteamAppSource::ProtonDb,
        _ => return None,
    };
    let mut parts = url.path_segments()?;
    if parts.next()? != "app" {
        return None;
    }
    let app_id = valid_steam_app_id(parts.next()?)?.parse::<u32>().ok()?;
    (parts.next().is_none()).then(|| (source, app_id.to_string()))
}

pub fn valid_steam_app_id(value: &str) -> Option<&str> {
    let app_id = value.trim();
    (app_id.len() <= 10
        && !app_id.is_empty()
        && app_id.bytes().all(|byte| byte.is_ascii_digit())
        && app_id.parse::<u32>().ok().is_some_and(|value| value > 0))
    .then_some(app_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spotify_resource_parser_accepts_only_canonical_urls() {
        assert_eq!(
            parse_spotify_resource_url(
                "https://open.spotify.com/track/4uLU6hMCjMI75M1A2tKUQC?si=share"
            ),
            Some((
                SpotifyResourceKind::Track,
                "4uLU6hMCjMI75M1A2tKUQC".to_string()
            ))
        );
        assert!(
            parse_spotify_resource_url("http://open.spotify.com/track/4uLU6hMCjMI75M1A2tKUQC")
                .is_none()
        );
        assert!(parse_spotify_resource_url("https://open.spotify.com/track/not-valid").is_none());
        assert!(
            parse_spotify_resource_url(
                "https://open.spotify.com/track/4uLU6hMCjMI75M1A2tKUQC/extra"
            )
            .is_none()
        );
        assert_eq!(
            crate::classify_link("https://open.spotify.com/track/4uLU6hMCjMI75M1A2tKUQC?si=share")
                .resource_key,
            crate::ResourceKey("spotify:track:4uLU6hMCjMI75M1A2tKUQC".to_string())
        );
    }

    #[test]
    fn steam_app_parser_accepts_only_canonical_urls() {
        assert_eq!(
            parse_steam_app_url("https://store.steampowered.com/app/730?l=english"),
            Some((SteamAppSource::Steam, "730".to_string()))
        );
        assert_eq!(
            parse_steam_app_url("https://steamdb.info/app/730"),
            Some((SteamAppSource::SteamDb, "730".to_string()))
        );
        assert!(parse_steam_app_url("http://store.steampowered.com/app/730").is_none());
        assert!(parse_steam_app_url("https://store.steampowered.com/app/0").is_none());
        assert!(parse_steam_app_url("https://store.steampowered.com/app/730/charts").is_none());
        assert_eq!(
            crate::classify_link("https://store.steampowered.com/app/730?l=english").resource_key,
            crate::ResourceKey("steam:app:730".to_string())
        );
    }
}
