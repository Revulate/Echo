//! Detection policy for chat art that depends on fixed-width wrapping.

const MIN_MULTILINE_LINES: usize = 3;
const MIN_TWITCH_WRAPPED_CHARS: usize = 48;
const SPECIAL_CHAR_DENSITY_PERCENT: usize = 30;
const ASCII_STRUCTURE_DENSITY_PERCENT: usize = 85;
const MIN_ASCII_PUNCTUATION_CHARS: usize = 12;
const TWITCH_CHAT_REFERENCE_WIDTH_PX: f32 = 340.0;

const BOX_DRAWING: &[char] = &[
    '─', '│', '┌', '┐', '└', '┘', '├', '┤', '┬', '┴', '┼', '═', '║', '╔', '╗', '╚', '╝', '╠', '╣',
    '╦', '╩', '╬', '━', '┃', '┏', '┓', '┗', '┛', '┣', '┫', '┳', '┻', '╋',
];

const BLOCK_CHARS: &[char] = &[
    '█', '▓', '▒', '░', '▄', '▀', '▌', '▐', '■', '□', '▪', '▫', '◆', '◇', '○', '●', '◎', '◉', '⬛',
    '⬜',
];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum AsciiArtKind {
    None,
    CollapsibleMultiline,
    TwitchWrapped,
}

impl AsciiArtKind {
    pub(crate) const fn uses_art_layout(self) -> bool {
        !matches!(self, Self::None)
    }

    pub(crate) const fn is_collapsible(self) -> bool {
        matches!(self, Self::CollapsibleMultiline)
    }

    pub(crate) fn layout_width(self, available_width: f32) -> f32 {
        if self.uses_art_layout() {
            available_width.min(TWITCH_CHAT_REFERENCE_WIDTH_PX)
        } else {
            available_width
        }
    }
}

/// Classify authored multi-line art and long art intended to soft-wrap at a
/// native Twitch chat width. Short decorative symbol messages remain ordinary.
pub(crate) fn classify(text: &str) -> AsciiArtKind {
    let line_count = text.lines().count();
    let mut total_chars = 0usize;
    let mut special_chars = 0usize;
    let mut ascii_punctuation_chars = 0usize;
    let mut ascii_structure_chars = 0usize;

    for character in text.chars() {
        total_chars += 1;
        if is_art_character(character) {
            special_chars += 1;
        }
        if character.is_ascii_punctuation() {
            ascii_punctuation_chars += 1;
            ascii_structure_chars += 1;
        } else if character.is_ascii_whitespace() {
            ascii_structure_chars += 1;
        }
    }

    if total_chars == 0 {
        return AsciiArtKind::None;
    }

    let has_special_density = special_chars * 100 >= total_chars * SPECIAL_CHAR_DENSITY_PERCENT;
    let has_ascii_structure_density = ascii_punctuation_chars >= MIN_ASCII_PUNCTUATION_CHARS
        && ascii_structure_chars * 100 >= total_chars * ASCII_STRUCTURE_DENSITY_PERCENT;

    if line_count >= MIN_MULTILINE_LINES {
        let repetitive_lines = text.lines().filter(|line| is_repetitive_line(line)).count();
        if has_special_density || has_ascii_structure_density || repetitive_lines * 2 > line_count {
            return AsciiArtKind::CollapsibleMultiline;
        }
    }

    if total_chars >= MIN_TWITCH_WRAPPED_CHARS
        && (has_special_density || has_ascii_structure_density || is_repetitive_line(text))
    {
        return AsciiArtKind::TwitchWrapped;
    }

    AsciiArtKind::None
}

#[inline]
fn is_art_character(character: char) -> bool {
    BOX_DRAWING.contains(&character)
        || BLOCK_CHARS.contains(&character)
        || matches!(character, '\u{2800}'..='\u{28ff}')
}

fn is_repetitive_line(line: &str) -> bool {
    let trimmed = line.trim();
    let mut characters = trimmed.chars();
    let Some(first) = characters.next() else {
        return false;
    };

    let mut character_count = 1usize;
    let mut matching_count = 1usize;
    for character in characters {
        character_count += 1;
        if character == first {
            matching_count += 1;
        }
    }

    character_count >= 8 && matching_count * 10 > character_count * 7
}
