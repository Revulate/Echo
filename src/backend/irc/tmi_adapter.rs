//! Allocation-aware formatting shared by Twitch IRC ingestion and chat replay.

use std::sync::Arc;

/// Format an RGB color as a lowercase `#rrggbb` string using a stack buffer.
#[inline]
pub fn format_color_rgb(r: u8, g: u8, b: u8) -> Arc<str> {
    const HEX_CHARS: &[u8; 16] = b"0123456789abcdef";
    let buf: [u8; 7] = [
        b'#',
        HEX_CHARS[(r >> 4) as usize],
        HEX_CHARS[(r & 0xf) as usize],
        HEX_CHARS[(g >> 4) as usize],
        HEX_CHARS[(g & 0xf) as usize],
        HEX_CHARS[(b >> 4) as usize],
        HEX_CHARS[(b & 0xf) as usize],
    ];
    // SAFETY: `buf` contains only ASCII bytes from the literal and `HEX_CHARS`.
    let s = unsafe { std::str::from_utf8_unchecked(&buf) };
    Arc::from(s)
}

/// Format a badge as `name/version`, avoiding a temporary string for small badges.
#[inline]
pub fn format_badge(name: &str, version: &str) -> Arc<str> {
    let total_len = name.len() + 1 + version.len();
    if total_len <= 64 {
        let mut buf = [0u8; 64];
        buf[..name.len()].copy_from_slice(name.as_bytes());
        buf[name.len()] = b'/';
        buf[name.len() + 1..total_len].copy_from_slice(version.as_bytes());
        // SAFETY: `name` and `version` are valid UTF-8 and `/` is ASCII.
        let s = unsafe { std::str::from_utf8_unchecked(&buf[..total_len]) };
        Arc::from(s)
    } else {
        Arc::from(format!("{name}/{version}").as_str())
    }
}
