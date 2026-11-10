//! Slot-based message parser (immediate emote resolution)
//!
//! Parses IRC messages into immutable slots with emotes resolved immediately
//! at parse time. If an emote isn't loaded, the word stays as text (no heuristics).
//!
//! Performance: ASCII chat uses a byte scanner (IRC emote tag positions are
//! byte-equivalent for ASCII), avoiding the char-position Vec allocation. The
//! Unicode path keeps char→byte mapping for emoji and non-ASCII emote ranges.

use echo_emote_core::Emote;
use rustc_hash::FxHashMap;
use std::borrow::Cow;
use std::sync::{
    Arc,
    atomic::{AtomicU32, Ordering},
};

use crate::slot::MessageSlot;
pub type TwitchEmoteMap = FxHashMap<usize, (String, usize)>;

pub trait SlotEmoteResolver {
    fn resolve(&self, name: &str) -> Option<Arc<Emote>>;

    fn on_resolved(&self, _name: &str, _emote: &Emote) {}

    fn on_unresolved(&self, _name: &str) {}
}

pub struct NoEmotes;

impl SlotEmoteResolver for NoEmotes {
    fn resolve(&self, _name: &str) -> Option<Arc<Emote>> {
        None
    }
}

/// Global embed ID counter for unique embed identification
static EMBED_ID_COUNTER: AtomicU32 = AtomicU32::new(0);

/// Generate a unique embed ID
fn next_embed_id() -> u32 {
    EMBED_ID_COUNTER.fetch_add(1, Ordering::Relaxed)
}

struct ResolvedEmoteToken<'a> {
    emote: Arc<Emote>,
    matched_word: &'a str,
    leading: &'a str,
    trailing: &'a str,
}

/// Resolve a composer token with the same precedence and case rules as the
/// message parser, without logging or scheduling provider refreshes.
pub fn resolve_input_emote_token(
    resolver: &dyn SlotEmoteResolver,
    word: &str,
) -> Option<Arc<Emote>> {
    // The parser classifies URLs and mentions before probing emote names.
    if normalize_url_token(word).is_some() || extract_mention_slice(word).is_some() {
        return None;
    }

    resolve_emote_token(resolver, word).map(|resolved| resolved.emote)
}

#[inline]
fn resolve_emote_token<'a>(
    resolver: &dyn SlotEmoteResolver,
    word: &'a str,
) -> Option<ResolvedEmoteToken<'a>> {
    // Exact matching must precede punctuation trimming because punctuation may
    // be part of an emote's case-sensitive name (for example, "D:" or "!joIN").
    if let Some(emote) = resolver.resolve(word) {
        return Some(ResolvedEmoteToken {
            emote,
            matched_word: word,
            leading: "",
            trailing: "",
        });
    }

    let (leading, candidate, trailing) = split_emote_token(word);
    if candidate == word || candidate.is_empty() {
        return None;
    }

    resolver.resolve(candidate).map(|emote| ResolvedEmoteToken {
        emote,
        matched_word: candidate,
        leading,
        trailing,
    })
}

// ============================================================================
// Slot Parser
// ============================================================================

/// Parse message text into immutable slots with emotes resolved.
///
/// All ingestion paths (history and live) resolve emotes synchronously here:
/// resolution is an in-memory lookup-map probe per token. The resulting slots
/// are the resident message binding: later provider catalog changes affect
/// future ingestion and later-loaded history, not this parsed row.
/// # Arguments
/// * `text` - The message text to parse
/// * `channel` - The channel name (for emote lookup - channel emotes take priority)
/// * `twitch_emotes` - Map of char position -> (emote_id, end_position) from IRC tags
///
/// # Returns
/// Vector of MessageSlots representing the message structure
pub fn parse_to_slots(
    text: &str,
    twitch_emotes: &TwitchEmoteMap,
    resolver: &dyn SlotEmoteResolver,
) -> Vec<MessageSlot> {
    if text.is_ascii() {
        return parse_ascii_to_slots(text, twitch_emotes, resolver);
    }

    // Build char position → (byte_offset, char) mapping once
    // This allows us to extract &str slices without per-word String allocations
    // Note: IRC emote positions use CHARACTER indices, not byte indices
    let char_positions: Vec<(usize, char)> = text.char_indices().collect();
    let char_count = char_positions.len();

    let mut slots = Vec::with_capacity(16);
    let mut i = 0;
    let mut current_text = String::new();

    while i < char_count {
        // Check for Twitch emote at this position (from IRC tags)
        if let Some((emote_id, end)) = twitch_emotes.get(&i)
            && (*end).min(char_count) > i
        {
            // Flush accumulated text
            flush_text(&mut current_text, &mut slots);

            // Extract emote text as &str slice (zero allocation)
            let end_clamped = (*end).min(char_count);
            let emote_text = get_slice(text, &char_positions, i, end_clamped);

            slots.push(MessageSlot::twitch_emote(emote_text, emote_id.as_str()));
            i = end_clamped;
            continue;
        }
        // Check for Unicode emoji at this position (rendered as Twemoji images)
        let byte_offset = char_positions[i].0;
        let remaining = &text[byte_offset..];
        if let Some((emoji_text, _)) = echo_text_core::emoji::emoji_prefix(remaining) {
            // Flush accumulated text before the emoji
            flush_text(&mut current_text, &mut slots);

            // Create emoji slot
            // Keep the exact source prefix for selection, copy, and range
            // accounting. The emoji table can canonicalize a sequence with an
            // inserted variation selector, so never use its spelling or the
            // entire remaining message here.
            slots.push(MessageSlot::emoji(emoji_text));

            // Advance by the source sequence only, so adjacent text and the
            // next emoji remain available to the scanner.
            let emoji_char_count = emoji_text.chars().count();
            i += emoji_char_count;
            continue;
        }

        // Check if we're at a word boundary for URL/mention/emote detection
        if is_word_start(i, &char_positions) {
            let word_end = find_word_end(&char_positions, i);
            // Extract word as &str slice (zero allocation)
            let word = get_slice(text, &char_positions, i, word_end);

            // Check for URL, including validated no-protocol domains.
            if let Some(url_token) = normalize_url_token(word) {
                flush_text(&mut current_text, &mut slots);
                if !url_token.leading.is_empty() {
                    slots.push(MessageSlot::text(url_token.leading));
                }
                slots.push(create_url_slot(url_token.url.as_ref()));
                if !url_token.trailing.is_empty() {
                    current_text.push_str(url_token.trailing);
                }
                i = word_end;
                // Add trailing space if present
                if word_end < char_count && char_positions[word_end].1 == ' ' {
                    current_text.push(' ');
                    i += 1;
                }
                continue;
            }

            // Check for @mention
            if word.starts_with('@')
                && word.len() > 1
                && let Some((username, username_char_len)) = extract_mention_slice(word)
            {
                flush_text(&mut current_text, &mut slots);
                slots.push(MessageSlot::mention(username));

                // Handle any trailing punctuation (username_char_len is char count, not bytes)
                let mention_char_len = username_char_len + 1; // +1 for @
                let word_char_len = word_end - i;
                if word_char_len > mention_char_len {
                    // Get trailing punctuation as slice
                    let trailing = get_slice(text, &char_positions, i + mention_char_len, word_end);
                    current_text.push_str(trailing);
                }

                i = word_end;
                // Add trailing space if present
                if word_end < char_count && char_positions[word_end].1 == ' ' {
                    current_text.push(' ');
                    i += 1;
                }
                continue;
            }

            if let Some(resolved) = resolve_emote_token(resolver, word) {
                resolver.on_resolved(resolved.matched_word, &resolved.emote);
                flush_text(&mut current_text, &mut slots);
                if !resolved.leading.is_empty() {
                    slots.push(MessageSlot::text(resolved.leading));
                }
                slots.push(MessageSlot::emote(resolved.emote));
                if !resolved.trailing.is_empty() {
                    current_text.push_str(resolved.trailing);
                }
                i = word_end;
                if word_end < char_count && char_positions[word_end].1 == ' ' {
                    current_text.push(' ');
                    i += 1;
                }
                continue;
            }

            resolver.on_unresolved(word);
        }

        // Regular character - accumulate into text
        current_text.push(char_positions[i].1);
        i += 1;
    }

    // Flush remaining text
    flush_text(&mut current_text, &mut slots);

    slots
}

// ============================================================================
// Helper Functions
// ============================================================================
fn parse_ascii_to_slots(
    text: &str,
    twitch_emotes: &TwitchEmoteMap,
    resolver: &dyn SlotEmoteResolver,
) -> Vec<MessageSlot> {
    let bytes = text.as_bytes();
    let mut slots = Vec::with_capacity(16);
    let mut i = 0usize;
    let mut current_text = String::new();
    let has_twitch_emotes = !twitch_emotes.is_empty();

    while i < bytes.len() {
        if has_twitch_emotes
            && let Some((emote_id, end)) = twitch_emotes.get(&i)
            && (*end).min(bytes.len()) > i
        {
            flush_text(&mut current_text, &mut slots);
            let end_clamped = (*end).min(bytes.len());
            slots.push(MessageSlot::twitch_emote(
                &text[i..end_clamped],
                emote_id.as_str(),
            ));
            i = end_clamped;
            continue;
        }

        if is_ascii_word_start(bytes, i) {
            let word_end = find_ascii_word_end(bytes, i);
            let word = &text[i..word_end];

            if let Some(url_token) = normalize_url_token(word) {
                flush_text(&mut current_text, &mut slots);
                if !url_token.leading.is_empty() {
                    slots.push(MessageSlot::text(url_token.leading));
                }
                slots.push(create_url_slot(url_token.url.as_ref()));
                if !url_token.trailing.is_empty() {
                    current_text.push_str(url_token.trailing);
                }
                i = word_end;
                if i < bytes.len() && bytes[i] == b' ' {
                    current_text.push(' ');
                    i += 1;
                }
                continue;
            }

            if word.starts_with('@')
                && word.len() > 1
                && let Some((username, username_len)) = extract_ascii_mention_slice(word)
            {
                flush_text(&mut current_text, &mut slots);
                slots.push(MessageSlot::mention(username));

                let mention_len = username_len + 1;
                if word.len() > mention_len {
                    current_text.push_str(&word[mention_len..]);
                }

                i = word_end;
                if i < bytes.len() && bytes[i] == b' ' {
                    current_text.push(' ');
                    i += 1;
                }
                continue;
            }

            if let Some(resolved) = resolve_emote_token(resolver, word) {
                resolver.on_resolved(resolved.matched_word, &resolved.emote);
                flush_text(&mut current_text, &mut slots);
                if !resolved.leading.is_empty() {
                    slots.push(MessageSlot::text(resolved.leading));
                }
                slots.push(MessageSlot::emote(resolved.emote));
                if !resolved.trailing.is_empty() {
                    current_text.push_str(resolved.trailing);
                }
                i = word_end;
                if i < bytes.len() && bytes[i] == b' ' {
                    current_text.push(' ');
                    i += 1;
                }
                continue;
            }

            resolver.on_unresolved(word);
        }

        current_text.push(bytes[i] as char);
        i += 1;
    }

    flush_text(&mut current_text, &mut slots);
    slots
}

#[inline]
fn is_ascii_word_start(bytes: &[u8], pos: usize) -> bool {
    pos == 0 || bytes[pos - 1].is_ascii_whitespace()
}

#[inline]
fn find_ascii_word_end(bytes: &[u8], start: usize) -> usize {
    let mut end = start;
    while end < bytes.len() && !bytes[end].is_ascii_whitespace() {
        end += 1;
    }
    end
}

fn extract_ascii_mention_slice(word: &str) -> Option<(&str, usize)> {
    let bytes = word.as_bytes();
    if bytes.first().copied() != Some(b'@') || bytes.len() <= 1 {
        return None;
    }

    let mut end = 1usize;
    while end < bytes.len() && (bytes[end].is_ascii_alphanumeric() || bytes[end] == b'_') {
        end += 1;
    }

    (end > 1).then_some((&word[1..end], end - 1))
}

/// Flush accumulated text into a Text slot
fn flush_text(current_text: &mut String, slots: &mut Vec<MessageSlot>) {
    if current_text.is_empty() {
        return;
    }

    // Trim trailing spaces for cleaner slots, but preserve structure
    let trimmed = current_text.trim_end();
    if !trimmed.is_empty() {
        slots.push(MessageSlot::text(trimmed));
    }

    current_text.clear();
}

/// Extract a &str slice from char positions (zero allocation)
///
/// # Arguments
/// * `text` - Original text string
/// * `char_positions` - Vec of (byte_offset, char) from char_indices()
/// * `start` - Start char position (inclusive)
/// * `end` - End char position (exclusive)
#[inline]
fn get_slice<'a>(
    text: &'a str,
    char_positions: &[(usize, char)],
    start: usize,
    end: usize,
) -> &'a str {
    if start >= char_positions.len() {
        return "";
    }
    let byte_start = char_positions[start].0;
    let byte_end = if end >= char_positions.len() {
        text.len()
    } else {
        char_positions[end].0
    };
    &text[byte_start..byte_end]
}

/// Check if position is at the start of a word
#[inline]
fn is_word_start(pos: usize, char_positions: &[(usize, char)]) -> bool {
    if pos == 0 {
        return true;
    }
    let (_, prev_char) = char_positions[pos - 1];
    prev_char.is_whitespace()
}

/// Find the end of a word starting at pos
#[inline]
fn find_word_end(char_positions: &[(usize, char)], start: usize) -> usize {
    let mut end = start;
    while end < char_positions.len() && !char_positions[end].1.is_whitespace() {
        end += 1;
    }
    end
}

/// Punctuation [`split_emote_token`] trims from either end of an emote word.
fn is_emote_affix_char(c: char) -> bool {
    matches!(
        c,
        '.' | ','
            | '!'
            | '?'
            | ':'
            | ';'
            | '"'
            | '\''
            | '('
            | ')'
            | '['
            | ']'
            | '{'
            | '}'
            | '<'
            | '>'
    )
}

/// Whether the emote slot at `emote_index` was typed glued to punctuation in
/// the text slot just before it, as in `(KEKW` or `hello !KEKW`.
///
/// The parser drops the whitespace before an emote and splits a leading affix
/// into its own text slot, so text shown in the emote's place needs that space
/// restored except after a glued affix. An affix-only text slot that follows
/// another text slot can only be a split-off leading affix, so it is glued.
/// After an emote, link or mention, or at the start of the message, the same
/// slot can also be trailing punctuation followed by a space (`KEKW, KEKW`),
/// which is far more common; only an opening bracket or quote counts as glued
/// there.
pub fn emote_slot_is_glued(slots: &[MessageSlot], emote_index: usize) -> bool {
    let Some(MessageSlot::Text(affix)) = emote_index
        .checked_sub(1)
        .and_then(|index| slots.get(index))
    else {
        return false;
    };
    if affix.is_empty() || !affix.chars().all(is_emote_affix_char) {
        return false;
    }
    match emote_index
        .checked_sub(2)
        .and_then(|index| slots.get(index))
    {
        Some(MessageSlot::Text(_)) => true,
        _ => affix.ends_with(['(', '[', '{', '<', '"', '\'']),
    }
}

/// Split a token into (leading punctuation, core token, trailing punctuation)
/// for emote matching. This preserves punctuation in text while matching the core.
fn split_emote_token(word: &str) -> (&str, &str, &str) {
    let core = word.trim_matches(is_emote_affix_char);
    if core.is_empty() || core.len() == word.len() {
        return ("", word, "");
    }

    let start = word.find(core).unwrap_or(0);
    let end = start + core.len();
    (&word[..start], &word[start..end], &word[end..])
}

/// Extract username from @mention as a &str slice (zero allocation)
/// Returns (username_slice, char_count) where char_count is the number of characters
/// in the username (for calculating trailing punctuation position)
fn extract_mention_slice(word: &str) -> Option<(&str, usize)> {
    if !word.starts_with('@') || word.len() <= 1 {
        return None;
    }

    // Skip the @ and count valid username characters
    let after_at = &word[1..];
    let mut char_count = 0;
    let mut byte_end = 0;

    for (i, c) in after_at.char_indices() {
        if c.is_ascii_alphanumeric() || c == '_' {
            char_count += 1;
            byte_end = i + c.len_utf8();
        } else {
            break;
        }
    }

    if char_count == 0 {
        None
    } else {
        Some((&after_at[..byte_end], char_count))
    }
}

/// Create URL slot with embed ID if embeddable
fn create_url_slot(url: &str) -> MessageSlot {
    // Every rendered URL has a stable per-occurrence id. Classification and
    // policy happen at the scheduling boundary, never while parsing a message.
    MessageSlot::Url {
        url: url.into(),
        embed_id: next_embed_id(),
    }
}

struct NormalizedUrlToken<'a> {
    leading: &'a str,
    url: Cow<'a, str>,
    trailing: &'a str,
}

fn normalize_url_token(word: &str) -> Option<NormalizedUrlToken<'_>> {
    let (leading, candidate, trailing) = split_url_token(word);
    if candidate.is_empty() {
        return None;
    }

    if candidate.starts_with("http://") || candidate.starts_with("https://") {
        return Some(NormalizedUrlToken {
            leading,
            url: Cow::Borrowed(candidate),
            trailing,
        });
    }

    if is_no_protocol_domain_url(candidate) {
        return Some(NormalizedUrlToken {
            leading,
            url: Cow::Owned(format!("https://{candidate}")),
            trailing,
        });
    }

    None
}

fn split_url_token(word: &str) -> (&str, &str, &str) {
    let start = word
        .char_indices()
        .find_map(|(idx, ch)| (!is_url_leading_punctuation(ch)).then_some(idx))
        .unwrap_or(word.len());

    let mut end = word.len();
    while end > start {
        let candidate = &word[start..end];
        let Some(ch) = candidate.chars().next_back() else {
            break;
        };
        if !should_trim_url_trailing_punctuation(candidate, ch) {
            break;
        }
        end -= ch.len_utf8();
    }

    (&word[..start], &word[start..end], &word[end..])
}

fn is_url_leading_punctuation(ch: char) -> bool {
    matches!(ch, '(' | '[' | '{' | '<' | '"') || ch == '\''
}

fn should_trim_url_trailing_punctuation(candidate: &str, ch: char) -> bool {
    match ch {
        '.' | ',' | '!' | '?' | ':' | ';' | '>' | '"' | '\'' => true,
        ')' => has_unmatched_url_closer(candidate, '(', ')'),
        ']' => has_unmatched_url_closer(candidate, '[', ']'),
        '}' => has_unmatched_url_closer(candidate, '{', '}'),
        _ => false,
    }
}

fn has_unmatched_url_closer(candidate: &str, opener: char, closer: char) -> bool {
    let mut balance = 0i32;
    for ch in candidate.chars() {
        if ch == opener {
            balance += 1;
        } else if ch == closer {
            balance -= 1;
        }
    }
    balance < 0
}

fn is_no_protocol_domain_url(word: &str) -> bool {
    if word.contains("://") || word.contains('@') {
        return false;
    }

    let host_end = word.find(['/', '?', '#']).unwrap_or(word.len());
    let host_port = &word[..host_end];
    let (host, port) = host_port
        .rsplit_once(':')
        .map(|(host, port)| (host, Some(port)))
        .unwrap_or((host_port, None));

    if let Some(port) = port
        && (port.is_empty() || !port.chars().all(|c| c.is_ascii_digit()))
    {
        return false;
    }

    if host.is_empty() || !host.contains('.') {
        return false;
    }

    let Some(tld) = host.rsplit('.').next() else {
        return false;
    };
    if tld.len() < 2 || !tld.chars().all(|c| c.is_ascii_alphabetic()) {
        return false;
    }

    host.split('.').all(is_valid_domain_label)
}

fn is_valid_domain_label(label: &str) -> bool {
    let mut chars = label.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    let last = label.chars().last().unwrap_or(first);

    first.is_ascii_alphanumeric()
        && last.is_ascii_alphanumeric()
        && label.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

/// Parse the IRC emotes tag format into a position map
/// Format: "emote_id:start-end,start-end/emote_id2:start-end"
///
/// Note: IRC positions are char indices (not byte indices)
pub fn parse_emotes_tag(tag: &str) -> TwitchEmoteMap {
    parse_emotes_tag_with_offset(tag, 0)
}

/// Parse the IRC emotes tag, shifting spans left by `strip_offset` and
/// dropping spans that fall entirely inside the stripped prefix (the leading
/// `@user ` mention Twitch prepends to reply messages).
///
/// Builds the final map in one pass instead of parsing the full map and
/// then filtering it into a second one.
pub fn parse_emotes_tag_with_offset(tag: &str, strip_offset: usize) -> TwitchEmoteMap {
    let mut result = FxHashMap::default();

    if tag.is_empty() {
        return result;
    }

    for emote_part in tag.split('/') {
        if let Some((id, ranges)) = emote_part.split_once(':') {
            for range in ranges.split(',') {
                if let Some((start, end)) = range.split_once('-')
                    && let (Ok(s), Ok(e)) = (start.parse::<usize>(), end.parse::<usize>())
                {
                    // End position is inclusive in IRC format, we need exclusive
                    let end_exclusive = e + 1;
                    if end_exclusive <= strip_offset {
                        continue;
                    }
                    let new_start = s.saturating_sub(strip_offset);
                    let new_end = end_exclusive.saturating_sub(strip_offset);
                    if new_end > new_start {
                        result.insert(new_start, (id.to_string(), new_end));
                    }
                }
            }
        }
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use echo_emote_core::EmoteProvider;

    struct TestEmotes(FxHashMap<String, Arc<Emote>>);

    impl SlotEmoteResolver for TestEmotes {
        fn resolve(&self, name: &str) -> Option<Arc<Emote>> {
            self.0.get(name).cloned()
        }
    }

    fn parse_without_emotes(text: &str, twitch_emotes: &TwitchEmoteMap) -> Vec<MessageSlot> {
        parse_to_slots(text, twitch_emotes, &NoEmotes)
    }

    fn test_emote(name: &str, zero_width: bool) -> Arc<Emote> {
        Arc::new(Emote {
            id: Arc::from(name),
            name: Arc::from(name),
            url: Arc::from("https://example.com/emote.webp"),
            provider: EmoteProvider::SevenTV,
            channel: None,
            original_name: None,
            creator_name: None,
            twitch_owner_login: None,
            twitch_owner_id: None,
            twitch_emote_type: None,
            url_1x: None,
            url_2x: None,
            url_4x: None,
            zero_width,
            is_animated: false,
            width: Some(28),
            height: Some(28),
        })
    }

    #[test]
    fn test_parse_simple_text() {
        let slots = parse_without_emotes("hello world", &FxHashMap::default());
        assert_eq!(slots.len(), 1);
        assert!(matches!(&slots[0], MessageSlot::Text(t) if t.as_ref() == "hello world"));
    }

    #[test]
    fn test_parse_with_twitch_emote() {
        let mut twitch_emotes = FxHashMap::default();
        twitch_emotes.insert(0, ("25".to_string(), 5)); // "Kappa" at position 0-4

        let slots = parse_without_emotes("Kappa hello", &twitch_emotes);

        assert_eq!(slots.len(), 2);
        // Twitch emotes from IRC tags become TwitchEmote slots
        assert!(matches!(&slots[0], MessageSlot::TwitchEmote { name, id }
            if name.as_ref() == "Kappa" && id.as_ref() == "25"));
    }

    #[test]
    fn test_parse_unknown_word_stays_text() {
        // With immediate resolution, unknown words stay as text (no heuristics)
        let slots = parse_without_emotes("hello PogChamp world", &FxHashMap::default());

        // PogChamp is NOT loaded in test context, so it stays as text
        assert_eq!(slots.len(), 1);
        assert!(matches!(&slots[0], MessageSlot::Text(t) if t.as_ref() == "hello PogChamp world"));
    }

    #[test]
    fn test_parse_url() {
        let slots = parse_without_emotes("check https://example.com out", &FxHashMap::default());

        assert_eq!(slots.len(), 3);
        assert!(
            matches!(&slots[1], MessageSlot::Url { url, .. } if url.as_ref() == "https://example.com")
        );
    }

    #[test]
    fn test_parse_www_url_without_protocol() {
        let slots = parse_without_emotes("check www.example.com/path out", &FxHashMap::default());

        assert_eq!(slots.len(), 3);
        assert!(
            matches!(&slots[1], MessageSlot::Url { url, .. } if url.as_ref() == "https://www.example.com/path")
        );
    }

    #[test]
    fn test_parse_mention() {
        let slots = parse_without_emotes("hello @user123 world", &FxHashMap::default());

        assert_eq!(slots.len(), 3);
        assert!(
            matches!(&slots[1], MessageSlot::Mention { username } if username.as_ref() == "user123")
        );
    }

    #[test]
    fn test_parse_emotes_tag() {
        let result = parse_emotes_tag("25:0-4,12-16/1902:6-10");

        assert_eq!(result.len(), 3);
        assert_eq!(result.get(&0), Some(&("25".to_string(), 5)));
        assert_eq!(result.get(&12), Some(&("25".to_string(), 17)));
        assert_eq!(result.get(&6), Some(&("1902".to_string(), 11)));
    }

    #[test]
    fn test_split_emote_token_with_punctuation() {
        assert_eq!(split_emote_token("Okayga,"), ("", "Okayga", ","));
        assert_eq!(split_emote_token("(Okayga)"), ("(", "Okayga", ")"));
        assert_eq!(split_emote_token("plain"), ("", "plain", ""));
    }

    #[test]
    fn utf8_twitch_emote_offsets_use_character_indices() {
        let mut twitch_emotes = TwitchEmoteMap::default();
        twitch_emotes.insert(2, ("25".to_string(), 7));
        let slots = parse_without_emotes("é Kappa", &twitch_emotes);
        assert!(matches!(&slots[0], MessageSlot::Text(text) if text.as_ref() == "é"));
        assert!(matches!(&slots[1], MessageSlot::TwitchEmote { name, id }
            if name.as_ref() == "Kappa" && id.as_ref() == "25"));
    }

    #[test]
    fn inline_emoji_keeps_adjacent_text_and_source_spelling() {
        let slots = parse_without_emotes("left😀right ♥tail", &TwitchEmoteMap::default());

        assert_eq!(slots.len(), 5);
        assert!(matches!(&slots[0], MessageSlot::Text(text) if text.as_ref() == "left"));
        assert!(matches!(&slots[1], MessageSlot::Emoji { emoji } if emoji.as_ref() == "😀"));
        assert!(matches!(&slots[2], MessageSlot::Text(text) if text.as_ref() == "right"));
        assert!(matches!(&slots[3], MessageSlot::Emoji { emoji } if emoji.as_ref() == "♥"));
        assert!(matches!(&slots[4], MessageSlot::Text(text) if text.as_ref() == "tail"));
    }

    #[test]
    fn consecutive_emoji_sequences_are_individual_slots() {
        let slots = parse_without_emotes("👋🎉🚀", &TwitchEmoteMap::default());

        assert_eq!(slots.len(), 3);
        assert!(matches!(&slots[0], MessageSlot::Emoji { emoji } if emoji.as_ref() == "👋"));
        assert!(matches!(&slots[1], MessageSlot::Emoji { emoji } if emoji.as_ref() == "🎉"));
        assert!(matches!(&slots[2], MessageSlot::Emoji { emoji } if emoji.as_ref() == "🚀"));
    }

    #[test]
    fn emoji_sequences_preserve_their_bounded_source_ranges() {
        let slots = parse_without_emotes("👨‍👩‍👧🇺🇸1️⃣", &TwitchEmoteMap::default());

        assert_eq!(slots.len(), 3);
        assert!(matches!(&slots[0], MessageSlot::Emoji { emoji } if emoji.as_ref() == "👨‍👩‍👧"));
        assert!(matches!(&slots[1], MessageSlot::Emoji { emoji } if emoji.as_ref() == "🇺🇸"));
        assert!(matches!(&slots[2], MessageSlot::Emoji { emoji } if emoji.as_ref() == "1️⃣"));
    }

    #[test]
    fn emoji_character_offsets_leave_following_twitch_emotes_aligned() {
        let mut twitch_emotes = TwitchEmoteMap::default();
        twitch_emotes.insert(1, ("25".to_string(), 6));

        let slots = parse_without_emotes("😀Kappa", &twitch_emotes);

        assert_eq!(slots.len(), 2);
        assert!(matches!(&slots[0], MessageSlot::Emoji { emoji } if emoji.as_ref() == "😀"));
        assert!(matches!(&slots[1], MessageSlot::TwitchEmote { name, id }
            if name.as_ref() == "Kappa" && id.as_ref() == "25"));
    }

    #[test]
    fn urls_and_mentions_keep_precedence_when_a_later_emoji_is_inline() {
        let slots = parse_without_emotes(
            "https://example.com @user 😀tail",
            &TwitchEmoteMap::default(),
        );

        assert_eq!(slots.len(), 4);
        assert!(
            matches!(&slots[0], MessageSlot::Url { url, .. } if url.as_ref() == "https://example.com")
        );
        assert!(
            matches!(&slots[1], MessageSlot::Mention { username } if username.as_ref() == "user")
        );
        assert!(matches!(&slots[2], MessageSlot::Emoji { emoji } if emoji.as_ref() == "😀"));
        assert!(matches!(&slots[3], MessageSlot::Text(text) if text.as_ref() == "tail"));
    }

    #[test]
    fn emote_slot_is_glued_tells_split_affixes_from_spaced_punctuation() {
        let kekw = test_emote("KEKW", false);
        let resolver = TestEmotes(FxHashMap::from_iter([("KEKW".to_string(), kekw)]));
        let glued = |text: &str| {
            let slots = parse_to_slots(text, &TwitchEmoteMap::default(), &resolver);
            let index = slots
                .iter()
                .rposition(|slot| matches!(slot, MessageSlot::Emote(_)))
                .expect("message has an emote");
            emote_slot_is_glued(&slots, index)
        };

        assert!(glued("(KEKW)"));
        assert!(glued("hello (KEKW"));
        assert!(glued("hello !KEKW"));
        assert!(glued("hello ,KEKW"));
        assert!(glued("KEKW (KEKW"));
        assert!(!glued("hello KEKW"));
        assert!(!glued("hello! KEKW"));
        assert!(!glued("lol, KEKW"));
        assert!(
            !glued("KEKW, KEKW"),
            "trailing punctuation of the first emote"
        );
        assert!(!glued("hello ! KEKW"));
        assert!(!glued("KEKW KEKW"));
        assert!(!glued("KEKW"));
    }

    #[test]
    fn emote_source_text_keeps_what_was_typed() {
        let mut twitch_emotes = TwitchEmoteMap::default();
        twitch_emotes.insert(0, ("1".to_string(), 3));
        let slots = parse_without_emotes(":-) hi", &twitch_emotes);
        assert_eq!(slots[0].emote_source_text(), Some(":-)"));
        assert_eq!(slots[1].emote_source_text(), None);
    }

    #[test]
    fn exact_emote_match_precedes_punctuation_trim() {
        let exact = test_emote("D:", false);
        let trimmed = test_emote("D", false);
        let resolver = TestEmotes(FxHashMap::from_iter([
            ("D:".to_string(), Arc::clone(&exact)),
            ("D".to_string(), trimmed),
        ]));
        let slots = parse_to_slots("D:", &TwitchEmoteMap::default(), &resolver);
        assert!(matches!(&slots[0], MessageSlot::Emote(emote) if Arc::ptr_eq(emote, &exact)));
    }

    #[test]
    fn url_classification_precedes_emote_resolution() {
        let url_emote = test_emote("example.com", false);
        let resolver = TestEmotes(FxHashMap::from_iter([(
            "example.com".to_string(),
            url_emote,
        )]));
        let slots = parse_to_slots("example.com", &TwitchEmoteMap::default(), &resolver);
        assert!(matches!(&slots[0], MessageSlot::Url { url, .. }
            if url.as_ref() == "https://example.com"));
    }

    #[test]
    fn resolved_zero_width_emote_preserves_arc() {
        let zero_width = test_emote("RainTime", true);
        let resolver = TestEmotes(FxHashMap::from_iter([(
            "RainTime".to_string(),
            Arc::clone(&zero_width),
        )]));
        let slots = parse_to_slots("RainTime", &TwitchEmoteMap::default(), &resolver);
        assert!(matches!(&slots[0], MessageSlot::Emote(emote)
            if emote.zero_width && Arc::ptr_eq(emote, &zero_width)));
    }
}
