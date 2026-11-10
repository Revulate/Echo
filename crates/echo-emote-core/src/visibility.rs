//! Pure emote-provider visibility decisions shared by runtime code and verification tools.

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EmoteProviderVisibilityKey {
    Twitch,
    SevenTv,
    Bttv,
    Ffz,
    Emoji,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EmoteProviderVisibility {
    pub seventv: bool,
    pub bttv: bool,
    pub ffz: bool,
}

impl Default for EmoteProviderVisibility {
    fn default() -> Self {
        Self {
            seventv: true,
            bttv: true,
            ffz: true,
        }
    }
}

pub fn emote_provider_visible(
    provider: EmoteProviderVisibilityKey,
    visibility: EmoteProviderVisibility,
) -> bool {
    match provider {
        EmoteProviderVisibilityKey::Twitch | EmoteProviderVisibilityKey::Emoji => true,
        EmoteProviderVisibilityKey::SevenTv => visibility.seventv,
        EmoteProviderVisibilityKey::Bttv => visibility.bttv,
        EmoteProviderVisibilityKey::Ffz => visibility.ffz,
    }
}
