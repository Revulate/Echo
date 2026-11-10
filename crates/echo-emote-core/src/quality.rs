use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
pub enum EmoteImageScale {
    #[default]
    #[serde(rename = "auto")]
    Auto,
    #[serde(rename = "1x")]
    OneX,
    #[serde(rename = "2x")]
    TwoX,
    #[serde(rename = "4x")]
    FourX,
}

impl EmoteImageScale {
    pub fn as_u8(self) -> u8 {
        match self {
            Self::Auto => 0,
            Self::OneX => 1,
            Self::TwoX => 2,
            Self::FourX => 4,
        }
    }

    pub fn from_u8(value: u8) -> Self {
        match value {
            1 => Self::OneX,
            2 => Self::TwoX,
            4 => Self::FourX,
            _ => Self::Auto,
        }
    }

    pub fn settings_value(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::OneX => "1x",
            Self::TwoX => "2x",
            Self::FourX => "4x",
        }
    }

    pub fn from_settings_value(value: &str) -> Self {
        match value.trim().to_ascii_lowercase().as_str() {
            "1x" | "low" | "performance" => Self::OneX,
            "2x" | "balanced" => Self::TwoX,
            "4x" | "best" | "quality" => Self::FourX,
            _ => Self::Auto,
        }
    }
}

/// Nominal logical height of a provider 1x emote in chat.
pub const NOMINAL_ONE_X_EMOTE_PX: f32 = 28.0;

/// Small source-density margin used by `Auto` so an asset is not painted at
/// the exact edge of its native resolution. Exact 1:1 sampling looked soft on
/// standard 1080p/100%-scale displays whenever placement landed between device
/// pixels. Twenty percent keeps the normal 28px chat row and the 46px picker on
/// 2x sources without promoting them to the substantially heavier 4x tier.
const AUTO_SOURCE_HEADROOM: f32 = 1.2;

/// Resolve `Auto` to the smallest provider tier that has enough source pixels
/// for the physical render size plus modest sampling headroom.
///
/// `rendered_scale` is the rendered logical height relative to a nominal 28px
/// 1x emote. Chat passes text zoom × emote zoom; larger surfaces such as the
/// picker pass their image height divided by [`NOMINAL_ONE_X_EMOTE_PX`]. The
/// resulting source choice stays display-scale-aware while avoiding a blanket
/// 4x policy: default 100% and 150% chat use 2x, while 4x is reserved for output
/// that exceeds the 2x tier's useful source density.
pub fn resolve_auto_tier(device_scale: f32, rendered_scale: f32) -> EmoteImageScale {
    let effective = device_scale.max(0.1) * rendered_scale.max(0.1) * AUTO_SOURCE_HEADROOM;
    if effective <= 1.001 {
        EmoteImageScale::OneX
    } else if effective <= 2.001 {
        EmoteImageScale::TwoX
    } else {
        EmoteImageScale::FourX
    }
}

pub fn select_emote_display_url<'a>(
    default_url: &'a str,
    url_1x: Option<&'a str>,
    url_2x: Option<&'a str>,
    url_4x: Option<&'a str>,
    scale: EmoteImageScale,
) -> &'a str {
    match scale {
        EmoteImageScale::OneX => url_1x.or(url_2x).or(url_4x).unwrap_or(default_url),
        EmoteImageScale::TwoX | EmoteImageScale::Auto => {
            url_2x.or(url_4x).or(url_1x).unwrap_or(default_url)
        }
        EmoteImageScale::FourX => url_4x.or(url_2x).or(url_1x).unwrap_or(default_url),
    }
}
