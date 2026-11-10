use serde::{Deserialize, Serialize};

/// Emoji style options.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
pub enum EmojiStyle {
    #[default]
    #[serde(rename = "twemoji")]
    Twemoji,
    #[serde(rename = "noto")]
    Noto,
    #[serde(rename = "native")]
    Native,
}
