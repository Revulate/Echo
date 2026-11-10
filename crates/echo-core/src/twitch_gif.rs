//! Twitch IRC `gifs` tag parsing.
//!
//! Twitch supplies the complete media URL in the tag and requires renderers to
//! use it without modification. Invalid metadata is intentionally ignored so
//! the authored message remains ordinary text rather than becoming an
//! arbitrary remote-media request.

use std::sync::Arc;

use crate::slot::MessageSlot;
use crate::slot_parser::{SlotEmoteResolver, TwitchEmoteMap, parse_to_slots};

const MAX_GIFS_PER_MESSAGE: usize = 8;
const MAX_GIFS_TAG_BYTES: usize = 8 * 1024;
const MAX_GIF_ID_BYTES: usize = 256;
const MAX_GIF_URL_BYTES: usize = 4 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
struct TwitchGifRange {
    start: usize,
    end: usize,
    id: Arc<str>,
    url: Arc<str>,
}

/// Parse regular message slots with validated Twitch GIF ranges substituted.
///
/// `source_offset` is the number of Unicode scalar positions removed before
/// this call, for example Echo's leading reply-mention strip. The Twitch tag
/// remains relative to the original IRC message.
pub fn parse_to_slots_with_gifs(
    text: &str,
    twitch_emotes: &TwitchEmoteMap,
    resolver: &dyn SlotEmoteResolver,
    gifs_tag: Option<&str>,
    source_offset: usize,
) -> Vec<MessageSlot> {
    let Some(gifs_tag) = gifs_tag.filter(|tag| !tag.is_empty()) else {
        return parse_to_slots(text, twitch_emotes, resolver);
    };

    let scalar_count = text.chars().count();
    let Some(gifs) = parse_gifs_tag(gifs_tag, scalar_count, source_offset) else {
        return parse_to_slots(text, twitch_emotes, resolver);
    };
    if gifs.is_empty() {
        return parse_to_slots(text, twitch_emotes, resolver);
    }

    let mut slots = Vec::with_capacity(gifs.len().saturating_mul(2).saturating_add(1));
    let mut cursor = 0;
    for gif in gifs {
        push_regular_slice(&mut slots, text, twitch_emotes, resolver, cursor, gif.start);
        let alt_text: Arc<str> = Arc::from(char_slice(text, gif.start, gif.end + 1));
        slots.push(MessageSlot::twitch_gif(gif.id, gif.url, alt_text));
        cursor = gif.end + 1;
    }
    push_regular_slice(
        &mut slots,
        text,
        twitch_emotes,
        resolver,
        cursor,
        scalar_count,
    );
    slots
}

fn push_regular_slice(
    output: &mut Vec<MessageSlot>,
    text: &str,
    twitch_emotes: &TwitchEmoteMap,
    resolver: &dyn SlotEmoteResolver,
    start: usize,
    end: usize,
) {
    if start >= end {
        return;
    }

    let slice = char_slice(text, start, end);
    let mut adjusted = TwitchEmoteMap::default();
    for (emote_start, (id, emote_end)) in twitch_emotes {
        if *emote_start >= start && *emote_end <= end {
            adjusted.insert(emote_start - start, (id.clone(), emote_end - start));
        }
    }
    output.extend(parse_to_slots(slice, &adjusted, resolver));
}

fn parse_gifs_tag(
    tag: &str,
    scalar_count: usize,
    source_offset: usize,
) -> Option<Vec<TwitchGifRange>> {
    if tag.len() > MAX_GIFS_TAG_BYTES {
        return None;
    }

    let items: Vec<_> = tag.split(',').collect();
    if items.is_empty() || items.len() > MAX_GIFS_PER_MESSAGE {
        return None;
    }

    let mut ranges = Vec::with_capacity(items.len());
    for item in items {
        let mut parts = item.split('|');
        let (Some(range), Some(id), Some(url), None) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            return None;
        };
        if id.is_empty() || id.len() > MAX_GIF_ID_BYTES || url.len() > MAX_GIF_URL_BYTES {
            return None;
        }
        let (Some(start), Some(end), None) = ({
            let mut positions = range.split('-');
            (positions.next(), positions.next(), positions.next())
        }) else {
            return None;
        };
        let (Ok(start), Ok(end)) = (start.parse::<usize>(), end.parse::<usize>()) else {
            return None;
        };
        if start > end || !is_giphy_https_url(url) {
            return None;
        }

        let (Some(start), Some(end)) = (
            start.checked_sub(source_offset),
            end.checked_sub(source_offset),
        ) else {
            return None;
        };
        if end >= scalar_count {
            return None;
        }
        ranges.push(TwitchGifRange {
            start,
            end,
            id: Arc::from(id),
            url: Arc::from(url),
        });
    }

    ranges.sort_by_key(|range| range.start);
    if ranges.windows(2).any(|pair| pair[0].end >= pair[1].start) {
        return None;
    }
    Some(ranges)
}

/// Allow exactly Giphy's HTTPS host or its DNS subdomains. The tag's path and
/// query remain opaque and are never normalized or rewritten.
fn is_giphy_https_url(value: &str) -> bool {
    // URL parsers can normalize backslashes and ASCII whitespace in ways that
    // change the apparent authority. Reject those spellings, never rewrite them.
    if value
        .bytes()
        .any(|byte| byte <= 0x20 || byte == 0x7f || byte == b'\\')
    {
        return false;
    }
    let Some(rest) = value.strip_prefix("https://") else {
        return false;
    };
    let authority_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let authority = &rest[..authority_end];
    if authority.is_empty()
        || !authority
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-'))
    {
        return false;
    }
    let host = authority.to_ascii_lowercase();
    host == "giphy.com" || host.ends_with(".giphy.com")
}

fn char_slice(text: &str, start: usize, end: usize) -> &str {
    let mut boundaries = text.char_indices().map(|(offset, _)| offset);
    let start_byte = boundaries.nth(start).unwrap_or(text.len());
    let end_byte = text
        .char_indices()
        .map(|(offset, _)| offset)
        .nth(end)
        .unwrap_or(text.len());
    &text[start_byte..end_byte]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::slot_parser::NoEmotes;

    #[test]
    fn gif_payload_keeps_regular_slot_size() {
        // Frozen pre-GIF slot shape: adding rare media must not inflate every
        // text/emote slot retained in scrollback.
        #[allow(dead_code)]
        enum BeforeGif {
            Text(Arc<str>),
            Emote(Arc<echo_emote_core::Emote>),
            TwitchEmote { id: Arc<str>, name: Arc<str> },
            Url { url: Arc<str>, embed_id: u32 },
            Mention { username: Arc<str> },
            Emoji { emoji: Arc<str> },
        }
        assert_eq!(
            std::mem::size_of::<MessageSlot>(),
            std::mem::size_of::<BeforeGif>()
        );
        println!(
            "message_slot_bytes: before={}, after={}",
            std::mem::size_of::<BeforeGif>(),
            std::mem::size_of::<MessageSlot>()
        );
    }

    #[test]
    fn preserves_full_official_url_and_caption() {
        let text = "[Y A Y Yes GIF by Djemilah Birnie]";
        let url =
            "https://media4.giphy.com/media/joSNxeswxuc74Juo8X/giphy.gif?cid=abc&rid=giphy.gif";
        let tag = format!("0-{}|joSNxeswxuc74Juo8X|{url}", text.chars().count() - 1);
        let slots =
            parse_to_slots_with_gifs(text, &TwitchEmoteMap::default(), &NoEmotes, Some(&tag), 0);

        assert!(matches!(slots.as_slice(), [MessageSlot::TwitchGif(gif)]
            if gif.id.as_ref() == "joSNxeswxuc74Juo8X" && gif.url.as_ref() == url && gif.alt_text.as_ref() == text));
    }

    #[test]
    fn supports_multiple_non_overlapping_ranges() {
        let text = "one two";
        let tag = "0-2|first|https://media1.giphy.com/media/first/giphy.gif,4-6|second|https://media2.giphy.com/media/second/giphy.gif";
        let slots =
            parse_to_slots_with_gifs(text, &TwitchEmoteMap::default(), &NoEmotes, Some(tag), 0);

        assert!(matches!(&slots[0], MessageSlot::TwitchGif(gif) if gif.alt_text.as_ref() == "one"));
        assert!(matches!(&slots[1], MessageSlot::TwitchGif(gif) if gif.alt_text.as_ref() == "two"));
    }

    #[test]
    fn malformed_or_untrusted_tags_leave_authored_text_alone() {
        let text = "[caption]";
        for tag in [
            "0-8|id|http://media.giphy.com/media/id/giphy.gif",
            "0-8|id|https://evil-giphy.com/media/id/giphy.gif",
            "0-8|id|https://127.0.0.1\\.giphy.com/media/id/giphy.gif",
            "0-8|id|https://media.giphy.com/has a space.gif",
            "0-8|id|https://media.giphy.com/media/id/giphy.gif,1-2|overlap|https://media.giphy.com/media/other/giphy.gif",
            "not-a-range|id|https://media.giphy.com/media/id/giphy.gif",
        ] {
            let slots =
                parse_to_slots_with_gifs(text, &TwitchEmoteMap::default(), &NoEmotes, Some(tag), 0);
            assert!(
                matches!(slots.as_slice(), [MessageSlot::Text(value)] if value.as_ref() == text)
            );
        }
    }

    #[test]
    fn reply_strip_offset_is_scalar_based() {
        let text = "[😀]";
        let tag = "2-4|id|https://media.giphy.com/media/id/giphy.gif";
        let slots =
            parse_to_slots_with_gifs(text, &TwitchEmoteMap::default(), &NoEmotes, Some(tag), 2);
        assert!(
            matches!(slots.as_slice(), [MessageSlot::TwitchGif(gif)] if gif.alt_text.as_ref() == text)
        );
    }

    #[test]
    fn preserves_twitch_emote_at_regular_slice_boundary() {
        let text = "Kappa [caption]";
        let mut emotes = TwitchEmoteMap::default();
        // The parser's end value is exclusive: `Kappa` occupies scalar 0..5.
        emotes.insert(0, ("25".to_string(), 5));
        let tag = "6-14|id|https://media.giphy.com/media/id/giphy.gif";
        let slots = parse_to_slots_with_gifs(text, &emotes, &NoEmotes, Some(tag), 0);
        assert!(
            matches!(slots.as_slice(), [MessageSlot::TwitchEmote { id, name }, MessageSlot::TwitchGif(_)]
            if id.as_ref() == "25" && name.as_ref() == "Kappa")
        );
    }
}
