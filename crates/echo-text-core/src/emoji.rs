//! Emoji Utilities
//!
//! Provides functions for detecting and rendering emoji characters as images.
//! Each [`EmojiStyle`] maps to a CDN image set (Twemoji or Noto).
//!
//! Uses the `emojis` crate for O(1) emoji lookup (Unicode 17.0 support).

use std::fmt::Write as _;

use smallvec::SmallVec;

use crate::EmojiStyle;

// ============================================================================
// CDN Version Constants
// ============================================================================

/// Twemoji version (Twitter's open-source emoji)
const TWEMOJI_VERSION: &str = "14.0.2";

/// emoji-datasource version for the Noto (Google) style
const EMOJI_DATASOURCE_VERSION: &str = "16.0.1";

// ============================================================================
// URL Generation
// ============================================================================

/// Convert emoji to CDN-compatible codepoints string (lowercase hex, dash-separated)
///
/// Filters out variation selectors (U+FE0F) as most CDNs don't include them in filenames.
fn emoji_to_codepoints(emoji: &str) -> String {
    let mut out = String::new();
    for c in emoji.chars().filter(|&c| c != '\u{FE0F}') {
        // Remove variation selector-16; hyphen-separate lowercase-hex codepoints.
        if !out.is_empty() {
            out.push('-');
        }
        let _ = write!(out, "{:x}", c as u32);
    }
    out
}

/// Get the CDN URL for an emoji based on the selected style
pub fn get_emoji_url(emoji: &str, style: &EmojiStyle) -> String {
    let codepoints = emoji_to_codepoints(emoji);

    match style {
        EmojiStyle::Twemoji => {
            // Twemoji CDN (version pinned to a verified asset set).
            format!(
                "https://cdnjs.cloudflare.com/ajax/libs/twemoji/{}/72x72/{}.png",
                TWEMOJI_VERSION, codepoints
            )
        }
        EmojiStyle::Noto => {
            format!(
                "https://cdn.jsdelivr.net/npm/emoji-datasource-google@{}/img/google/64/{}.png",
                EMOJI_DATASOURCE_VERSION, codepoints
            )
        }
        EmojiStyle::Native => {
            // NOTE: Do not use inline data:image SVG for "native" style.
            // The image loader/render path does not reliably decode that variant and can
            // leave a small placeholder block in chat (seen with 💤). Prefer a real CDN URL.
            format!(
                "https://cdnjs.cloudflare.com/ajax/libs/twemoji/{}/72x72/{}.png",
                TWEMOJI_VERSION, codepoints
            )
        }
    }
}

/// Upper bound for a fully-qualified Unicode emoji sequence in the locked
/// `emojis` data set, with room for a future sequence extension.
const MAX_EMOJI_SEQUENCE_BYTES: usize = 64;

#[inline]
fn is_regional_indicator(c: char) -> bool {
    (0x1F1E6..=0x1F1FF).contains(&(c as u32))
}

#[inline]
fn is_keycap_base(c: char) -> bool {
    matches!(c, '#' | '*' | '0'..='9')
}

#[inline]
fn is_emoji_sequence_component(c: char) -> bool {
    matches!(
        c,
        '\u{200D}' | '\u{FE0E}' | '\u{FE0F}' | '\u{20E3}' | '\u{E0020}'..='\u{E007F}'
    ) || (0x1F3FB..=0x1F3FF).contains(&(c as u32))
}

/// Return the longest emoji-table entry at the beginning of `remaining`.
///
/// `emojis::get` accepts complete sequences, not a prefix of a longer message.
/// Passing the entire remainder therefore misses ordinary inline emoji and led
/// the former broad Unicode-block fallback to turn non-emoji symbols such as
/// `✧` into empty image slots. This scans only the bounded continuation forms
/// of a table-backed first scalar, preserving flags, keycaps, variation
/// selectors, skin tones, and ZWJ emoji while leaving ordinary symbols as text.
pub fn emoji_prefix(remaining: &str) -> Option<(&str, &'static emojis::Emoji)> {
    let first = remaining.chars().next()?;
    let first_end = first.len_utf8();

    // Regional indicators are valid only as a two-scalar flag pair. Treating
    // them separately avoids looking past one flag into later chat text.
    if is_regional_indicator(first) {
        let second = remaining[first_end..].chars().next()?;
        if !is_regional_indicator(second) {
            return None;
        }
        let end = first_end + second.len_utf8();
        return emojis::get(&remaining[..end]).map(|emoji| (&remaining[..end], emoji));
    }

    // Keycaps start from an otherwise ordinary ASCII character, so they do not
    // have a one-scalar entry to seed the general sequence path below.
    if is_keycap_base(first) {
        let mut end = first_end;
        if remaining[end..].starts_with('\u{FE0F}') {
            end += '\u{FE0F}'.len_utf8();
        }
        if remaining[end..].starts_with('\u{20E3}') {
            end += '\u{20E3}'.len_utf8();
            return emojis::get(&remaining[..end]).map(|emoji| (&remaining[..end], emoji));
        }
        return None;
    }

    let mut matched =
        emojis::get(&remaining[..first_end]).map(|emoji| (&remaining[..first_end], emoji))?;
    let mut end = first_end;
    let mut expects_joined_base = false;

    while end < remaining.len() && end < MAX_EMOJI_SEQUENCE_BYTES {
        let next = remaining[end..].chars().next()?;
        if !expects_joined_base && !is_emoji_sequence_component(next) {
            break;
        }

        end += next.len_utf8();
        if end > MAX_EMOJI_SEQUENCE_BYTES {
            break;
        }

        if let Some(emoji) = emojis::get(&remaining[..end]) {
            matched = (&remaining[..end], emoji);
        }
        expects_joined_base = next == '\u{200D}';
    }

    Some(matched)
}

// ============================================================================
// Text Parsing
// ============================================================================

/// Segment of text with optional emoji
#[derive(Debug, Clone)]
pub enum TextSegment {
    /// Plain text
    Text(String),
    /// Emoji character with CDN URL and metadata
    Emoji {
        /// The emoji character(s)
        char: String,
        /// CDN URL for the emoji image
        url: String,
        /// Human-readable name (e.g., "waving hand")
        name: String,
    },
}

/// Parse text and split into text and emoji segments using bounded table lookup
///
/// This is significantly faster than regex-based detection, especially for
/// long messages with many potential emoji positions.
pub fn parse_emoji(text: &str, style: &EmojiStyle) -> SmallVec<[TextSegment; 4]> {
    let mut segments: SmallVec<[TextSegment; 4]> = SmallVec::new();
    let mut text_buffer = String::new();
    let mut chars = text.char_indices().peekable();

    while let Some((idx, _)) = chars.peek().copied() {
        let remaining = &text[idx..];

        if let Some((emoji_text, emoji)) = emoji_prefix(remaining) {
            // Flush accumulated text before the emoji
            if !text_buffer.is_empty() {
                segments.push(TextSegment::Text(std::mem::take(&mut text_buffer)));
            }

            // Add emoji segment
            segments.push(TextSegment::Emoji {
                char: emoji_text.to_string(),
                url: get_emoji_url(emoji_text, style),
                name: emoji.name().to_string(),
            });

            // Skip exactly the source sequence. `emojis::get` can return a
            // fully-qualified form with an inserted variation selector, which
            // must not make us consume the following text character.
            let emoji_len = emoji_text.len();
            let mut skipped = 0;
            while skipped < emoji_len {
                if let Some((_, c)) = chars.next() {
                    skipped += c.len_utf8();
                } else {
                    break;
                }
            }
        } else if let Some((_, c)) = chars.peek().copied() {
            // Not a table-backed emoji, so preserve the source character as text.
            chars.next();
            text_buffer.push(c);
        } else {
            break;
        }
    }

    // Flush remaining text
    if !text_buffer.is_empty() {
        segments.push(TextSegment::Text(text_buffer));
    }

    // Ensure we return at least one segment
    if segments.is_empty() {
        segments.push(TextSegment::Text(text.to_string()));
    }

    segments
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_emoji_basic() {
        let segments = parse_emoji("Hello 👋 World", &EmojiStyle::Twemoji);
        assert_eq!(segments.len(), 3);

        assert!(
            matches!(&segments[0], TextSegment::Text(t) if t == "Hello "),
            "Expected text segment with 'Hello ', got {:?}",
            segments[0]
        );

        assert!(
            matches!(&segments[1], TextSegment::Emoji { char, name, .. } if char == "👋" && !name.is_empty()),
            "Expected emoji segment with '👋', got {:?}",
            segments[1]
        );

        assert!(
            matches!(&segments[2], TextSegment::Text(t) if t == " World"),
            "Expected text segment with ' World', got {:?}",
            segments[2]
        );
    }

    #[test]
    fn test_parse_emoji_zwj_sequence() {
        // ZWJ sequences like family emoji should be detected as single emoji
        let segments = parse_emoji("Family: 👨‍👩‍👧", &EmojiStyle::Twemoji);

        // Should have: "Family: " text + family emoji
        assert_eq!(segments.len(), 2);
        assert!(matches!(&segments[0], TextSegment::Text(t) if t == "Family: "));
        assert!(matches!(&segments[1], TextSegment::Emoji { char, .. } if char.contains("👨")));
    }

    #[test]
    fn test_parse_emoji_flags() {
        // Flag emojis are two regional indicator symbols
        let segments = parse_emoji("US: 🇺🇸", &EmojiStyle::Twemoji);

        assert_eq!(segments.len(), 2);
        assert!(matches!(&segments[0], TextSegment::Text(t) if t == "US: "));
        assert!(matches!(&segments[1], TextSegment::Emoji { char, .. } if char == "🇺🇸"));
    }

    #[test]
    fn test_parse_emoji_multiple() {
        let segments = parse_emoji("👋🎉🚀", &EmojiStyle::Twemoji);

        // Three consecutive emoji with no text between
        assert_eq!(segments.len(), 3);
        assert!(matches!(&segments[0], TextSegment::Emoji { char, .. } if char == "👋"));
        assert!(matches!(&segments[1], TextSegment::Emoji { char, .. } if char == "🎉"));
        assert!(matches!(&segments[2], TextSegment::Emoji { char, .. } if char == "🚀"));
    }

    #[test]
    fn test_parse_emoji_no_emoji() {
        let segments = parse_emoji("Hello World", &EmojiStyle::Twemoji);

        assert_eq!(segments.len(), 1);
        assert!(matches!(&segments[0], TextSegment::Text(t) if t == "Hello World"));
    }

    #[test]
    fn test_parse_emoji_empty() {
        let segments = parse_emoji("", &EmojiStyle::Twemoji);

        assert_eq!(segments.len(), 1);
        assert!(matches!(&segments[0], TextSegment::Text(t) if t.is_empty()));
    }

    #[test]
    fn test_emoji_url_twemoji() {
        let url = get_emoji_url("👋", &EmojiStyle::Twemoji);
        assert!(url.contains("twemoji"));
        assert!(url.contains("1f44b")); // 👋 codepoint
        assert!(url.contains(TWEMOJI_VERSION));
    }

    #[test]
    fn test_emoji_url_noto() {
        let url = get_emoji_url("😀", &EmojiStyle::Noto);
        assert!(url.contains("emoji-datasource-google"));
        assert!(url.contains("1f600")); // 😀 codepoint
        assert!(url.contains(EMOJI_DATASOURCE_VERSION));
    }

    #[test]
    fn test_emoji_codepoints_filters_variation_selector() {
        // 👋️ with variation selector should become just "1f44b"
        let codepoints = emoji_to_codepoints("👋\u{FE0F}");
        assert_eq!(codepoints, "1f44b");
        assert!(!codepoints.contains("fe0f"));
    }
}
