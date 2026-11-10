use serde::{Deserialize, Serialize};

/// Emoji style options.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
pub enum EmojiStyle {
    #[default]
    #[serde(rename = "twemoji")]
    Twemoji,
    #[serde(rename = "noto")]
    Noto,
    /// Renders the Twemoji image set (see `emoji::get_emoji_url`); the
    /// variant keeps its name so saved settings still load.
    #[serde(rename = "native")]
    Native,
}
