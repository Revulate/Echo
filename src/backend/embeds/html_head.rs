//! Head-only scanner for link-preview metadata.
//!
//! Link previews read a handful of `<meta>` tags (OpenGraph, Twitter, the
//! description) and the `<title>` from untrusted pages. This scanner reads
//! only those, from the start of the document up to `</head>` or
//! [`MAX_SCAN_BYTES`], in one pass over the bytes: no tree, no allocation per
//! tag beyond the values it keeps, and no input can make it do more than a
//! constant amount of work per byte.
//!
//! It follows the HTML tokenizer where that changes which value a selector
//! like `meta[property='og:title']` would find: attribute names are matched
//! without case and attribute values with it, the first matching tag wins
//! (even when it has no `content`), the first copy of a repeated attribute
//! wins, quoted values may contain `>`, comments (ended by `-->` or `--!>`)
//! and the raw-text elements (`script`, `style`, `noscript` with scripting
//! on, and the rest) hide the tags inside them, and a tag cut off by the end
//! of the input is dropped. Values are decoded from the document's charset
//! (UTF-8, or windows-1252 for the labels that mean it; anything else is read
//! as UTF-8 with replacement characters), then character references are
//! decoded with the full WHATWG named-reference list (`html_entities.rs`).
//! Each value is decoded from at most its first [`MAX_VALUE_BYTES`].
//!
//! Known differences from a full HTML parser, all deliberate: nothing after
//! `</head>` is read; `<svg>`/`<math>` foreign content, `<frameset>` and
//! script double-escaping (`<script><!--<script>`) are not modelled.

/// Bytes scanned at most, whatever the caller passes.
pub(crate) const MAX_SCAN_BYTES: usize = 512 * 1024;

/// `meta[property=...]` keys the previews read.
const PROPERTY_KEYS: [&str; 6] = [
    "og:title",
    "og:image",
    "og:video:secure_url",
    "og:video",
    "og:description",
    "og:site_name",
];

/// `meta[name=...]` keys the previews read.
const NAME_KEYS: [&str; 5] = [
    "twitter:title",
    "twitter:site",
    "twitter:image",
    "description",
    "application-name",
];

/// Attributes the scan keeps from a tag.
const KEPT_ATTRIBUTES: [&str; 5] = ["property", "name", "content", "charset", "http-equiv"];

/// Raw-text elements whose contents are not markup.
const RAW_TEXT_ELEMENTS: [&str; 8] = [
    "script", "style", "noscript", "noframes", "iframe", "xmp", "noembed", "textarea",
];

/// Metadata found in a document's head. A key maps to the `content` of the
/// first `<meta>` carrying it; that tag may have had no `content`.
#[derive(Debug, Default, PartialEq)]
pub(crate) struct HeadMetadata {
    properties: [Option<Option<String>>; PROPERTY_KEYS.len()],
    names: [Option<Option<String>>; NAME_KEYS.len()],
    title: Option<String>,
}

impl HeadMetadata {
    /// `content` of the first `<meta property="key">`.
    pub(crate) fn property(&self, key: &str) -> Option<&str> {
        let index = PROPERTY_KEYS.iter().position(|known| *known == key)?;
        self.properties[index].as_ref()?.as_deref()
    }

    /// `content` of the first `<meta name="key">`.
    pub(crate) fn name(&self, key: &str) -> Option<&str> {
        let index = NAME_KEYS.iter().position(|known| *known == key)?;
        self.names[index].as_ref()?.as_deref()
    }

    /// Text of the first `<title>`, untrimmed.
    pub(crate) fn title(&self) -> Option<&str> {
        self.title.as_deref()
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Charset {
    Utf8,
    Windows1252,
}

fn charset_from_label(label: &str) -> Option<Charset> {
    let label = label
        .trim_matches(|c: char| c.is_ascii_whitespace() || c == '"' || c == '\'')
        .to_ascii_lowercase();
    match label.as_str() {
        "utf-8" | "utf8" | "unicode-1-1-utf-8" | "x-unicode20utf8" => Some(Charset::Utf8),
        "windows-1252" | "cp1252" | "x-cp1252" | "iso-8859-1" | "iso8859-1" | "iso_8859-1"
        | "iso88591" | "iso_8859-1:1987" | "latin1" | "l1" | "ibm819" | "cp819" | "csisolatin1"
        | "us-ascii" | "ascii" | "ansi_x3.4-1968" | "iso-ir-100" => Some(Charset::Windows1252),
        _ => None,
    }
}

/// The `charset=` parameter of a Content-Type value, if any.
pub(crate) fn content_type_charset(content_type: &str) -> Option<&str> {
    let lowered = content_type.to_ascii_lowercase();
    let start = lowered.find("charset")? + "charset".len();
    let rest = content_type[start..].trim_start();
    let rest = rest.strip_prefix('=')?.trim_start();
    let rest = rest.trim_start_matches(['"', '\'']);
    let end = rest
        .find(|c: char| c == '"' || c == '\'' || c == ';' || c.is_ascii_whitespace())
        .unwrap_or(rest.len());
    (end > 0).then(|| &rest[..end])
}

/// A value's bytes in the input, decoded once the charset is known.
#[derive(Clone, Copy)]
struct Span {
    start: usize,
    end: usize,
}

struct Attribute {
    name: Span,
    value: Span,
}

fn is_space(byte: u8) -> bool {
    matches!(byte, b'\t' | b'\n' | 0x0C | b'\r' | b' ')
}

fn eq_ignore_case(input: &[u8], span: Span, expected: &str) -> bool {
    input[span.start..span.end].eq_ignore_ascii_case(expected.as_bytes())
}

/// Scan `html` (the start of a document) for the preview metadata.
/// `header_charset` is the Content-Type header's charset, if it had one.
pub(crate) fn scan_head(html: &[u8], header_charset: Option<&str>) -> HeadMetadata {
    let input = &html[..html.len().min(MAX_SCAN_BYTES)];
    let mut raw = RawHead::default();
    scan(input, &mut raw);

    let charset = if input.starts_with(b"\xEF\xBB\xBF") {
        Charset::Utf8
    } else {
        header_charset
            .and_then(charset_from_label)
            .or_else(|| {
                let span = raw.meta_charset?;
                charset_from_label(&String::from_utf8_lossy(&input[span.start..span.end]))
            })
            .unwrap_or(Charset::Utf8)
    };

    let attribute = |span: Option<Span>| {
        span.map(|span| {
            let span = capped_value_span(input, span, charset);
            decode_references(&decode_bytes(input, span, charset), true)
        })
    };
    let mut metadata = HeadMetadata::default();
    for (slot, found) in metadata.properties.iter_mut().zip(raw.properties) {
        *slot = found.map(attribute);
    }
    for (slot, found) in metadata.names.iter_mut().zip(raw.names) {
        *slot = found.map(attribute);
    }
    metadata.title = raw.title.map(|span| {
        let span = capped_value_span(input, span, charset);
        decode_references(&decode_bytes(input, span, charset), false)
    });
    metadata
}

/// Bytes of one value that are decoded at most. Previews show the first 100
/// to 200 characters, so the rest of a long value is never needed, and the
/// cap bounds the decoding work and allocation a crafted page can cause.
const MAX_VALUE_BYTES: usize = 2048;

/// `span` cut to [`MAX_VALUE_BYTES`], ending on a character boundary in UTF-8.
fn capped_value_span(input: &[u8], span: Span, charset: Charset) -> Span {
    if span.end - span.start <= MAX_VALUE_BYTES {
        return span;
    }
    let mut end = span.start + MAX_VALUE_BYTES;
    if charset == Charset::Utf8 {
        while end > span.start && input[end] & 0xC0 == 0x80 {
            end -= 1;
        }
    }
    Span {
        start: span.start,
        end,
    }
}

/// What the scan found, as spans into the input.
#[derive(Default)]
struct RawHead {
    properties: [Option<Option<Span>>; PROPERTY_KEYS.len()],
    names: [Option<Option<Span>>; NAME_KEYS.len()],
    title: Option<Span>,
    meta_charset: Option<Span>,
}

fn find_byte(input: &[u8], from: usize, byte: u8) -> Option<usize> {
    input
        .get(from..)?
        .iter()
        .position(|&b| b == byte)
        .map(|at| from + at)
}

/// Index just after the `-->` or `--!>` that ends a comment whose text starts
/// at `from`.
fn find_comment_end(input: &[u8], from: usize) -> Option<usize> {
    let mut at = from;
    while let Some(dashes) = find_bytes(input, at, b"--") {
        match input.get(dashes + 2..) {
            Some([b'>', ..]) => return Some(dashes + 3),
            Some([b'!', b'>', ..]) => return Some(dashes + 4),
            _ => at = dashes + 1,
        }
    }
    None
}

fn find_bytes(input: &[u8], from: usize, needle: &[u8]) -> Option<usize> {
    input
        .get(from..)?
        .windows(needle.len())
        .position(|window| window == needle)
        .map(|at| from + at)
}

/// Start of the first `</name` (any case) at or after `from` that is
/// followed by whitespace, `/` or `>`, the only end tag that closes a raw-text
/// or RCDATA element.
fn find_end_tag(input: &[u8], from: usize, name: &str) -> Option<usize> {
    let mut at = from;
    while let Some(open) = find_bytes(input, at, b"</") {
        let name_start = open + 2;
        let name_end = name_start + name.len();
        if input
            .get(name_start..name_end)
            .is_some_and(|candidate| candidate.eq_ignore_ascii_case(name.as_bytes()))
            && input
                .get(name_end)
                .is_some_and(|&b| is_space(b) || b == b'/' || b == b'>')
        {
            return Some(open);
        }
        at = name_start;
    }
    None
}

fn scan(input: &[u8], raw: &mut RawHead) {
    let mut attributes = Vec::new();
    let mut i = 0;
    while let Some(open) = find_byte(input, i, b'<') {
        let rest = &input[open..];
        if rest.starts_with(b"<!--") {
            if rest.starts_with(b"<!-->") {
                i = open + 5;
                continue;
            }
            if rest.starts_with(b"<!--->") {
                i = open + 6;
                continue;
            }
            match find_comment_end(input, open + 4) {
                Some(end) => i = end,
                None => return,
            }
            continue;
        }
        let Some(&next) = rest.get(1) else {
            return;
        };
        if next == b'!' || next == b'?' {
            match find_byte(input, open + 2, b'>') {
                Some(end) => i = end + 1,
                None => return,
            }
            continue;
        }
        let end_tag = next == b'/';
        let name_start = if end_tag { open + 2 } else { open + 1 };
        if !input.get(name_start).is_some_and(u8::is_ascii_alphabetic) {
            if end_tag {
                // `</>` is dropped; `</` before anything else but a letter
                // starts a bogus comment that ends at the next `>`.
                match find_byte(input, name_start, b'>') {
                    Some(end) => i = end + 1,
                    None => return,
                }
            } else {
                i = open + 1;
            }
            continue;
        }
        let mut name_end = name_start;
        while name_end < input.len()
            && !is_space(input[name_end])
            && !matches!(input[name_end], b'/' | b'>')
        {
            name_end += 1;
        }
        let name = Span {
            start: name_start,
            end: name_end,
        };
        attributes.clear();
        let Some(after_tag) = parse_attributes(input, name_end, &mut attributes) else {
            return;
        };
        if end_tag {
            if eq_ignore_case(input, name, "head") {
                return;
            }
            i = after_tag;
            continue;
        }
        if eq_ignore_case(input, name, "meta") {
            record_meta(input, &attributes, raw);
        } else if eq_ignore_case(input, name, "title") {
            let end = find_end_tag(input, after_tag, "title");
            if raw.title.is_none() {
                raw.title = Some(Span {
                    start: after_tag,
                    end: end.unwrap_or(input.len()),
                });
            }
            match end {
                Some(end) => {
                    i = end;
                    continue;
                }
                None => return,
            }
        } else if eq_ignore_case(input, name, "plaintext") {
            return;
        } else if let Some(element) = RAW_TEXT_ELEMENTS
            .iter()
            .find(|element| eq_ignore_case(input, name, element))
        {
            match find_end_tag(input, after_tag, element) {
                Some(end) => {
                    i = end;
                    continue;
                }
                None => return,
            }
        }
        i = after_tag;
    }
}

/// Parse a tag's attributes from `from` (just after its name) through its
/// closing `>`. Returns the index after the `>`, or `None` when the input
/// ends inside the tag (the tokenizer drops such a tag).
fn parse_attributes(input: &[u8], from: usize, attributes: &mut Vec<Attribute>) -> Option<usize> {
    let mut i = from;
    loop {
        while i < input.len() && (is_space(input[i]) || input[i] == b'/') {
            i += 1;
        }
        let &byte = input.get(i)?;
        if byte == b'>' {
            return Some(i + 1);
        }
        // An attribute name runs to whitespace, `/`, `>` or `=` (a leading
        // `=` belongs to the name).
        let name_start = i;
        i += 1;
        while i < input.len() && !is_space(input[i]) && !matches!(input[i], b'/' | b'>' | b'=') {
            i += 1;
        }
        let name = Span {
            start: name_start,
            end: i,
        };
        while i < input.len() && is_space(input[i]) {
            i += 1;
        }
        let mut value = Span { start: i, end: i };
        if input.get(i) == Some(&b'=') {
            i += 1;
            while i < input.len() && is_space(input[i]) {
                i += 1;
            }
            match *input.get(i)? {
                quote @ (b'"' | b'\'') => {
                    let end = find_byte(input, i + 1, quote)?;
                    value = Span { start: i + 1, end };
                    i = end + 1;
                }
                b'>' => {}
                _ => {
                    let start = i;
                    while i < input.len() && !is_space(input[i]) && input[i] != b'>' {
                        i += 1;
                    }
                    value = Span { start, end: i };
                }
            }
        }
        // Only the few attributes `<meta>` is read for are kept, so a tag
        // with thousands of attributes costs no more than one with five. The
        // first copy of a repeated attribute wins.
        let wanted = KEPT_ATTRIBUTES
            .iter()
            .any(|kept| eq_ignore_case(input, name, kept));
        let repeated = || {
            attributes.iter().any(|existing: &Attribute| {
                input[existing.name.start..existing.name.end]
                    .eq_ignore_ascii_case(&input[name.start..name.end])
            })
        };
        if wanted && !repeated() {
            attributes.push(Attribute { name, value });
        }
    }
}

fn record_meta(input: &[u8], attributes: &[Attribute], raw: &mut RawHead) {
    let attribute = |wanted: &str| {
        attributes
            .iter()
            .find(|attribute| eq_ignore_case(input, attribute.name, wanted))
            .map(|attribute| attribute.value)
    };
    let content = attribute("content");
    if let Some(property) = attribute("property") {
        let value = &input[property.start..property.end];
        if let Some(index) = PROPERTY_KEYS.iter().position(|key| key.as_bytes() == value)
            && raw.properties[index].is_none()
        {
            raw.properties[index] = Some(content);
        }
    }
    if let Some(name) = attribute("name") {
        let value = &input[name.start..name.end];
        if let Some(index) = NAME_KEYS.iter().position(|key| key.as_bytes() == value)
            && raw.names[index].is_none()
        {
            raw.names[index] = Some(content);
        }
    }
    if raw.meta_charset.is_none() {
        if let Some(charset) = attribute("charset") {
            raw.meta_charset = Some(charset);
        } else if let (Some(http_equiv), Some(content)) = (attribute("http-equiv"), content)
            && eq_ignore_case(input, http_equiv, "content-type")
            && let Ok(text) = std::str::from_utf8(&input[content.start..content.end])
            && let Some(label) = content_type_charset(text)
        {
            // Point the span at the label inside `content`.
            let offset = label.as_ptr() as usize - text.as_ptr() as usize;
            raw.meta_charset = Some(Span {
                start: content.start + offset,
                end: content.start + offset + label.len(),
            });
        }
    }
}

/// windows-1252 characters for bytes 0x80..=0x9F.
const WINDOWS_1252_HIGH: [char; 32] = [
    '\u{20AC}', '\u{0081}', '\u{201A}', '\u{0192}', '\u{201E}', '\u{2026}', '\u{2020}', '\u{2021}',
    '\u{02C6}', '\u{2030}', '\u{0160}', '\u{2039}', '\u{0152}', '\u{008D}', '\u{017D}', '\u{008F}',
    '\u{0090}', '\u{2018}', '\u{2019}', '\u{201C}', '\u{201D}', '\u{2022}', '\u{2013}', '\u{2014}',
    '\u{02DC}', '\u{2122}', '\u{0161}', '\u{203A}', '\u{0153}', '\u{009D}', '\u{017E}', '\u{0178}',
];

/// Decode a span from the document charset, with the tokenizer's input
/// normalization: CR and CRLF become LF, NUL becomes U+FFFD.
fn decode_bytes(input: &[u8], span: Span, charset: Charset) -> String {
    let bytes = &input[span.start..span.end];
    let text: String = match charset {
        Charset::Utf8 => String::from_utf8_lossy(bytes).into_owned(),
        Charset::Windows1252 => bytes
            .iter()
            .map(|&byte| match byte {
                0x80..=0x9F => WINDOWS_1252_HIGH[usize::from(byte - 0x80)],
                _ => char::from(byte),
            })
            .collect(),
    };
    if !text.contains(['\r', '\0']) {
        return text;
    }
    text.replace("\r\n", "\n")
        .replace('\r', "\n")
        .replace('\0', "\u{FFFD}")
}

/// The characters and legacy flag of the named reference `name` (without
/// `&` or `;`), from the full WHATWG list.
fn named_reference(name: &str) -> Option<(&'static str, bool)> {
    use super::html_entities::NAMED_REFERENCES;
    NAMED_REFERENCES
        .binary_search_by(|(candidate, ..)| candidate.as_bytes().cmp(name.as_bytes()))
        .ok()
        .map(|index| (NAMED_REFERENCES[index].1, NAMED_REFERENCES[index].2))
}

/// Longest legacy name (one that matches without `;`): `frac12`, `middot`.
const LONGEST_LEGACY_NAME: usize = 6;

/// Characters numeric references to 0x80..=0x9F stand for (windows-1252).
fn numeric_reference(value: u32) -> char {
    match value {
        0 | 0xD800..=0xDFFF | 0x11_0000.. => '\u{FFFD}',
        0x80..=0x9F => WINDOWS_1252_HIGH[(value - 0x80) as usize],
        _ => char::from_u32(value).unwrap_or('\u{FFFD}'),
    }
}

/// Decode character references in `text`. In an attribute value a legacy
/// name without `;` followed by an alphanumeric or `=` stays as written.
fn decode_references(text: &str, in_attribute: bool) -> String {
    if !text.contains('&') {
        return text.to_string();
    }
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut copied = 0;
    let mut i = 0;
    while let Some(at) = text[i..].find('&').map(|at| i + at) {
        out.push_str(&text[copied..at]);
        copied = at;
        i = at + 1;
        if bytes.get(i) == Some(&b'#') {
            let hex = matches!(bytes.get(i + 1), Some(b'x' | b'X'));
            let digits_start = if hex { i + 2 } else { i + 1 };
            let mut j = digits_start;
            let mut value: u32 = 0;
            while let Some(&b) = bytes.get(j) {
                let digit = if hex {
                    (b as char).to_digit(16)
                } else {
                    (b as char).to_digit(10)
                };
                let Some(digit) = digit else {
                    break;
                };
                value = value
                    .saturating_mul(if hex { 16 } else { 10 })
                    .saturating_add(digit);
                j += 1;
            }
            if j == digits_start {
                continue;
            }
            if bytes.get(j) == Some(&b';') {
                j += 1;
            }
            out.push(numeric_reference(value));
            copied = j;
            i = j;
            continue;
        }
        let mut j = i;
        while j < bytes.len() && j - i < 32 && bytes[j].is_ascii_alphanumeric() {
            j += 1;
        }
        let run = &text[i..j];
        if bytes.get(j) == Some(&b';')
            && let Some((value, _)) = named_reference(run)
        {
            out.push_str(value);
            copied = j + 1;
            i = j + 1;
            continue;
        }
        // The longest legacy name the run starts with, matched without `;`.
        let legacy = (2..=run.len().min(LONGEST_LEGACY_NAME))
            .rev()
            .find_map(|len| match named_reference(&run[..len]) {
                Some((value, true)) => Some((len, value)),
                _ => None,
            });
        if let Some((name_len, value)) = legacy {
            let after = i + name_len;
            let blocked = in_attribute
                && bytes
                    .get(after)
                    .is_some_and(|&b| b.is_ascii_alphanumeric() || b == b'=');
            if !blocked {
                out.push_str(value);
                copied = after;
                i = after;
            }
        }
    }
    out.push_str(&text[copied..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test;

    fn scan(html: &str) -> HeadMetadata {
        scan_head(html.as_bytes(), None)
    }

    #[test]
    fn attributes_in_any_order_and_quoting() {
        let head = scan(concat!(
            "<!DOCTYPE html><html><HEAD>",
            "<META CONTENT='Single quoted' PROPERTY=og:title>",
            "<meta content=unquoted/value name=description>",
            "<meta\tproperty=\"og:image\"\ncontent = \"https://example.com/a.png?x=1&amp;y=2\" />",
            "<meta name=\"twitter:site\" content=\"@echo\">",
            "<title>  A &amp; B </title>",
            "</head><body></body></html>",
        ));
        assert_eq!(head.property("og:title"), Some("Single quoted"));
        assert_eq!(head.name("description"), Some("unquoted/value"));
        assert_eq!(
            head.property("og:image"),
            Some("https://example.com/a.png?x=1&y=2")
        );
        assert_eq!(head.name("twitter:site"), Some("@echo"));
        assert_eq!(head.title(), Some("  A & B "));
    }

    #[test]
    fn property_and_name_are_separate_and_values_match_with_case() {
        let head = scan(concat!(
            "<meta name=\"og:title\" content=\"named\">",
            "<meta property=\"OG:TITLE\" content=\"shouting\">",
            "<meta property=\"description\" content=\"property\">",
            "<meta property=\"og:title\" content=\"right\">",
        ));
        assert_eq!(head.property("og:title"), Some("right"));
        assert_eq!(head.name("og:title"), None);
        assert_eq!(head.name("description"), None);
    }

    #[test]
    fn first_tag_and_first_attribute_win() {
        let head = scan(concat!(
            "<meta property=\"og:title\" content=\"first\" content=\"repeat\">",
            "<meta property=\"og:title\" content=\"second\">",
            "<meta property=\"og:description\">",
            "<meta property=\"og:description\" content=\"later\">",
            "<title>One</title><title>Two</title>",
        ));
        assert_eq!(head.property("og:title"), Some("first"));
        // The first matching tag has no content, as a selector would find.
        assert_eq!(head.property("og:description"), None);
        assert_eq!(head.title(), Some("One"));
    }

    #[test]
    fn comments_scripts_and_quoted_markup_hide_tags() {
        let head = scan(concat!(
            "<!-- <meta property=\"og:title\" content=\"comment\"> </head> -->",
            "<script>var s = '</head><meta property=\"og:title\" content=\"script\">';</script>",
            "<noscript><meta property=\"og:title\" content=\"noscript\"></noscript>",
            "<style>/* </style */ </STYLE>",
            "<meta name=\"description\" content=\"a > b\">",
            "<meta property=\"og:title\" content=\"real\">",
        ));
        assert_eq!(head.property("og:title"), Some("real"));
        assert_eq!(head.name("description"), Some("a > b"));
    }

    #[test]
    fn scanning_stops_at_the_end_of_the_head_or_the_cap() {
        let head = scan(concat!(
            "<head><meta property=\"og:title\" content=\"head\"></head>",
            "<body><meta property=\"og:image\" content=\"body.png\"><title>Late</title>",
        ));
        assert_eq!(head.property("og:title"), Some("head"));
        assert_eq!(head.property("og:image"), None);
        assert_eq!(head.title(), None);

        // Without </head> the scan goes on to the cap.
        let head = scan("<head><title>T</title><body><meta name=\"description\" content=\"d\">");
        assert_eq!(head.name("description"), Some("d"));

        let mut padded = String::from("<head><title>Padded</title>");
        padded.push_str(&" ".repeat(MAX_SCAN_BYTES));
        padded.push_str("<meta property=\"og:title\" content=\"too late\">");
        let head = scan(&padded);
        assert_eq!(head.property("og:title"), None);
        assert_eq!(head.title(), Some("Padded"));
    }

    #[test]
    fn character_references_decode_like_the_tokenizer() {
        let head = scan(concat!(
            "<meta property=\"og:title\" content=\"&#39;Q&#x27; &eacute;t&eacute; &hellip; &#150; &#0; &bogus; &amp\">",
            "<meta name=\"description\" content=\"a=1&ampb=2&copy=3 &lt x\">",
            "<title>&lt;b&gt; &ampx &#x1F600;</title>",
        ));
        assert_eq!(
            head.property("og:title"),
            Some("'Q' été … \u{2013} \u{FFFD} &bogus; &")
        );
        assert_eq!(head.name("description"), Some("a=1&ampb=2&copy=3 < x"));
        assert_eq!(head.title(), Some("<b> &x \u{1F600}"));
    }

    #[test]
    fn charset_comes_from_the_header_or_a_meta_tag() {
        let latin1 = b"<head><meta charset=\"iso-8859-1\"><title>Caf\xE9 \x93q\x94</title></head>";
        assert_eq!(
            scan_head(latin1, None).title(),
            Some("Café \u{201C}q\u{201D}")
        );
        let declared = b"<meta http-equiv=\"Content-Type\" content=\"text/html; charset=windows-1252\"><title>\xE9</title>";
        assert_eq!(scan_head(declared, None).title(), Some("é"));
        let header_wins = b"<meta charset=utf-8><title>\xE9</title>";
        assert_eq!(scan_head(header_wins, Some("latin1")).title(), Some("é"));
        // Invalid UTF-8 is replaced, never rejected.
        let invalid = b"<title>ok \xFF\xFE end</title>";
        assert_eq!(
            scan_head(invalid, None).title(),
            Some("ok \u{FFFD}\u{FFFD} end")
        );
        // A charset this scanner does not know is read as UTF-8.
        let unknown = b"<meta charset=shift_jis><title>\xE2\x9C\x93</title>";
        assert_eq!(scan_head(unknown, None).title(), Some("\u{2713}"));
        assert_eq!(
            content_type_charset("text/html; Charset=\"UTF-8\""),
            Some("UTF-8")
        );
        assert_eq!(content_type_charset("text/html"), None);
    }

    #[test]
    fn names_missing_a_semicolon_use_the_full_reference_list() {
        // Full names that begin with a legacy name decode as themselves.
        let head = scan("<title>a &notin; b &parallel; c &centerdot; d &ltimes; e &acE;</title>");
        assert_eq!(
            head.title(),
            Some("a \u{2209} b \u{2225} c \u{B7} d \u{22C9} e \u{223E}\u{333}")
        );
        // An unknown name decodes only its legacy prefix in text, as the
        // tokenizer does, and stays as written in an attribute.
        let head = scan(concat!(
            "<meta property=\"og:title\" content=\"&notit; &spades; &frac13;\">",
            "<title>&notit; &frac12x &CounterClockwiseContourIntegral;</title>",
        ));
        assert_eq!(head.property("og:title"), Some("&notit; \u{2660} \u{2153}"));
        assert_eq!(head.title(), Some("\u{AC}it; \u{BD}x \u{2233}"));
    }

    #[test]
    fn the_reference_table_is_sorted_and_its_legacy_names_are_short() {
        use super::super::html_entities::NAMED_REFERENCES;
        assert_eq!(NAMED_REFERENCES.len(), 2125);
        assert!(
            NAMED_REFERENCES
                .windows(2)
                .all(|pair| pair[0].0 < pair[1].0)
        );
        let legacy: Vec<&str> = NAMED_REFERENCES
            .iter()
            .filter(|(_, _, legacy)| *legacy)
            .map(|(name, ..)| *name)
            .collect();
        assert_eq!(legacy.len(), 106);
        assert!(
            legacy
                .iter()
                .all(|name| (2..=LONGEST_LEGACY_NAME).contains(&name.len()))
        );
    }

    #[test]
    fn comments_end_at_either_closing_form() {
        let head = scan("<!----!><meta name=description content=X>");
        assert_eq!(head.name("description"), Some("X"));
        let head = scan("<!-- a -- b --!> <meta property=og:title content=T> <!-- x -->");
        assert_eq!(head.property("og:title"), Some("T"));
        let head = scan("<!-- <meta property=og:title content=hidden> --!");
        assert_eq!(head.property("og:title"), None);
    }

    #[test]
    fn long_values_are_decoded_from_their_first_bytes_only() {
        let long = "\u{E9}".repeat(MAX_VALUE_BYTES);
        let html = format!(
            "<meta property=\"og:title\" content=\"x{long}\"><title>{}</title>",
            "&amp;".repeat(MAX_VALUE_BYTES)
        );
        let head = scan(&html);
        let title = head.property("og:title").unwrap();
        assert!(title.len() <= MAX_VALUE_BYTES);
        assert!(!title.contains('\u{FFFD}'));
        assert!(head.title().unwrap().len() <= MAX_VALUE_BYTES);

        // A crafted page of references stays cheap: the decoded span is capped.
        let crafted = format!(
            "<meta property=og:title name=description content=\"{}\"><title>{}",
            "&a".repeat(MAX_SCAN_BYTES / 4),
            "&a".repeat(MAX_SCAN_BYTES / 4)
        );
        let head = scan(&crafted);
        assert!(head.property("og:title").unwrap().len() <= MAX_VALUE_BYTES);
    }

    /// Cases from the review's adversarial pass (scratchpad `hh/src/adv.rs`),
    /// with the tokenizer's answers.
    #[test]
    fn adversarial_cases_from_review() {
        let head = scan("<head><!-- x --!><meta property=og:title content=Visible></head>");
        assert_eq!(head.property("og:title"), Some("Visible"));

        let head = scan(concat!(
            "<title>a &notin; b &parallel; c &centerdot; d &ltimes; e &spades; f</title>",
            "<meta property=og:title content=\"x &notin; y &copysr; z\">",
        ));
        assert_eq!(
            head.title(),
            Some("a \u{2209} b \u{2225} c \u{B7} d \u{22C9} e \u{2660} f")
        );
        assert_eq!(head.property("og:title"), Some("x \u{2209} y \u{2117} z"));

        for (html, image) in [
            (
                "<head><meta property=og:title content=A></head ><meta property=og:image content=B>",
                None,
            ),
            (
                "<head><meta property=og:title content=A></HEAD><meta property=og:image content=B>",
                None,
            ),
            (
                "<head><meta property=og:title content=A></head/><meta property=og:image content=B>",
                None,
            ),
            (
                "<head><meta property=og:title content=A></head x='>'><meta property=og:image content=B>",
                None,
            ),
            (
                "<head><meta property=og:title content=A></header><meta property=og:image content=B>",
                Some("B"),
            ),
            (
                "<head><meta property=og:title content=A><textarea></head></textarea><meta property=og:image content=B>",
                Some("B"),
            ),
        ] {
            let head = scan(html);
            assert_eq!(head.property("og:title"), Some("A"), "{html}");
            assert_eq!(head.property("og:image"), image, "{html}");
        }

        let head = scan("<head><noscript><meta property=og:image content=/ns.png></noscript>");
        assert_eq!(head.property("og:image"), None);

        for (html, title) in [
            ("<title>&</title>", "&"),
            ("<title>&#</title>", "&#"),
            ("<title>&#x</title>", "&#x"),
            ("<title>&#x;</title>", "&#x;"),
            (
                "<title>&\u{E9} &#\u{E9} &amp\u{E9} &amp;\u{E9}</title>",
                "&\u{E9} &#\u{E9} &\u{E9} &\u{E9}",
            ),
        ] {
            assert_eq!(scan(html).title(), Some(title), "{html}");
        }
        let _ = scan("<meta property=og:title content=\"&\u{1F600}&#x\u{1F600};&lt\u{1F600}\">");
        for content_type in [
            "text/html; charset=\u{212A}oi8",
            "charset",
            "charset=",
            "\u{17F}charset=utf-8",
            "text/html;charset=\"\"",
            "\u{C4} charset = 'latin1'",
        ] {
            let _ = content_type_charset(content_type);
        }
        let multibyte = "<meta http-equiv=content-type content=\"\u{FC}; charset=latin1\"><title>\u{E9}</title>";
        // The content is not plain ASCII but is valid UTF-8, so the label is read.
        assert_eq!(scan(multibyte).title(), Some("\u{C3}\u{A9}"));

        let bom = scan_head(
            b"\xEF\xBB\xBF<meta charset=latin1><title>\xC3\xA9</title>",
            None,
        );
        assert_eq!(bom.title(), Some("\u{E9}"));
        let header = scan_head(
            b"<meta charset=latin1><title>\xC3\xA9</title>",
            Some("utf-8"),
        );
        assert_eq!(header.title(), Some("\u{E9}"));
        let unknown = scan_head(
            b"<meta charset=latin1><title>\xC3\xA9</title>",
            Some("shift_jis"),
        );
        assert_eq!(unknown.title(), Some("\u{C3}\u{A9}"));
    }

    /// The review's worst-case shapes, each a full 512 KB: they must finish
    /// (linear work, capped decoding) and give the expected value.
    #[test]
    fn worst_case_inputs_finish() {
        let n = MAX_SCAN_BYTES;
        let fill = |prefix: &str, unit: &str, suffix: &str| {
            let mut html = String::from(prefix);
            while html.len() < n - suffix.len() - unit.len() {
                html.push_str(unit);
            }
            html.push_str(suffix);
            html
        };
        let head = scan(&fill(
            "<meta property=og:title name=description content=\"",
            "&a",
            "\">",
        ));
        assert!(
            head.property("og:title")
                .is_some_and(|value| value.len() <= MAX_VALUE_BYTES)
        );
        let _ = scan(&fill(
            "<meta property=og:title content=\"",
            "&aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "\">",
        ));
        assert!(scan(&fill("<title>", "&a", "")).title().is_some());
        let _ = scan(&fill("", "<a>", ""));
        let _ = scan(&fill("<meta", " a", ">"));
        let _ = scan(&fill("<meta", " content=x name=y", ">"));
        let _ = scan(&fill("<script>", "</scrip", ""));
        let _ = scan(&fill("<!--", "--", ""));
        let _ = scan(&fill("", "</ ", ""));
        let _ = scan(&fill("", "<<", ""));
        let _ = scan(&fill("<meta property=og:title content=\"", "&#9;", "\">"));
        let mut latin = b"<meta charset=latin1><title>".to_vec();
        while latin.len() < n {
            latin.extend_from_slice(b"\r\x80\0");
        }
        assert!(scan_head(&latin, None).title().is_some());
    }

    #[test]
    fn malformed_markup_never_panics_or_loops() {
        for html in [
            "",
            "<",
            "<<<<",
            "</",
            "</>",
            "<!",
            "<!--",
            "<!-->",
            "<meta",
            "<meta property=\"og:title\" content=\"unterminated",
            "<meta property=og:title content=",
            "<meta =x ==y property=og:title content=v>",
            "<meta charset=utf-8 http-equiv=content-type content=\"text/html; charset=ÿ\">",
            "<title>never closed",
            "<script>never closed <meta property=og:title content=x>",
            "&#xFFFFFFFFFFFF; &#99999999999999999999;",
            "<meta property=\"og:title\" content=\"&#xFFFFFFFFFFFF;&#55296;\">",
            "<\u{0}meta>\r\n<title>a\r\nb\rc\0</title>",
        ] {
            let _ = scan(html);
        }
        // A tag the input cuts off is dropped.
        assert_eq!(
            scan("<meta property=\"og:title\" content=\"unterminated").property("og:title"),
            None
        );
        assert_eq!(
            scan("<meta property=og:title content=x").property("og:title"),
            None
        );
        assert_eq!(
            scan("<meta property=\"og:title\" content=\"&#xFFFFFFFFFFFF;&#55296;\">")
                .property("og:title"),
            Some("\u{FFFD}\u{FFFD}")
        );
        assert_eq!(
            scan("<title>a\r\nb\rc\0</title>").title(),
            Some("a\nb\nc\u{FFFD}")
        );
        assert_eq!(scan("<title>never closed").title(), Some("never closed"));
    }
}
