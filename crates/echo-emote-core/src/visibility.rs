//! Pure emote-provider visibility decisions shared by runtime code and verification tools.

use crate::EmoteProvider;

/// Which third-party emote providers the user has enabled. Twitch emotes and
/// emoji are always shown.
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
    provider: EmoteProvider,
    visibility: EmoteProviderVisibility,
) -> bool {
    match provider {
        EmoteProvider::Twitch | EmoteProvider::Emoji => true,
        EmoteProvider::SevenTV => visibility.seventv,
        EmoteProvider::BTTV => visibility.bttv,
        EmoteProvider::FFZ => visibility.ffz,
    }
}
