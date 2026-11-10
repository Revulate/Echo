//! Color parsing utilities for Echo
//!
//! Provides unified hex color parsing.

use gpui::Hsla;

/// Parse a Twitch-supplied user color and pull it into a readable range for the
/// active theme.
///
/// Every surface that renders a username must go through this. The canvas
/// renderer normalizes inside its own color cache, so a surface that parsed the
/// raw hex instead would show the same user in a different color than chat --
/// and on a light theme it would show them in the near-invisible original.
/// The normalized value comes from that same theme-keyed cache, since the
/// readability search is too costly to repeat on every render.
pub fn parse_user_color_opt(hex: &str) -> Option<Hsla> {
    parse_hex_color_opt(hex)?;
    Some(crate::components::chat::canvas_renderer::layout::get_cached_color(hex))
}

/// Parse a hex color string, returning Option<Hsla>.
/// Returns None if the input is invalid.
pub fn parse_hex_color_opt(hex: &str) -> Option<Hsla> {
    let digits = hex.trim_start_matches('#');
    if digits.len() != 6 {
        return None;
    }
    crate::theme::colors::hex_to_hsla(digits)
}

/// Parse optional hex color with fallback to specified default
/// Common pattern: parse_optional_hex_or(&user.color, colors::text_primary())
/// PERF: Accepts Option<Arc<str>> to avoid allocation when used with ChatMessage.color
#[inline]
pub fn parse_optional_hex_or(color: &Option<std::sync::Arc<str>>, default: Hsla) -> Hsla {
    color
        .as_ref()
        .and_then(|c| parse_hex_color_opt(c.as_ref()))
        .unwrap_or(default)
}

/// Official provider brand colors
pub mod provider_colors {
    use gpui::Hsla;

    /// Twitch purple (#9146FF)
    pub const TWITCH: Hsla = Hsla {
        h: 0.74,
        s: 1.0,
        l: 0.64,
        a: 1.0,
    };

    /// 7TV teal (#29D1A3) - official brand color
    pub const SEVENTV: Hsla = Hsla {
        h: 0.45,
        s: 0.70,
        l: 0.50,
        a: 1.0,
    };

    /// BetterTTV red (#D2322D)
    pub const BTTV: Hsla = Hsla {
        h: 0.01,
        s: 0.67,
        l: 0.50,
        a: 1.0,
    };

    /// FrankerFaceZ green (#659C55)
    pub const FFZ: Hsla = Hsla {
        h: 0.30,
        s: 0.30,
        l: 0.47,
        a: 1.0,
    };
}

/// Get primary color for emote provider by name
pub fn emote_provider_color(provider: &str) -> Hsla {
    match provider {
        "Twitch" => provider_colors::TWITCH,
        "7TV" => provider_colors::SEVENTV,
        "BTTV" | "BetterTTV" => provider_colors::BTTV,
        "FFZ" | "FrankerFaceZ" => provider_colors::FFZ,
        _ => crate::theme::colors::text_muted(),
    }
}
