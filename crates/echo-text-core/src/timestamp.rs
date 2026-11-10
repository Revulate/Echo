//! Pure timestamp pattern validation and formatting.

use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// Legacy message timestamp formats.
///
/// Echo stores an ordered [`TimestampPattern`] instead. This enum survives
/// only as an explicit compatibility decoder for settings files written before
/// the token model: it is accepted on load, migrated exactly once through
/// [`MessageTimestampFormat::into_pattern`], and never written back.
///
/// The serialized values use Qt format spelling (`h:mm a`, `hh:mm:ss.zzz`).
/// Visibility is stored separately, so the selected format is remembered while
/// timestamps are disabled.
#[repr(u8)]
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash, Default)]
pub enum MessageTimestampFormat {
    #[default]
    #[serde(rename = "h:mm")]
    HourMinute = 0,
    #[serde(rename = "hh:mm")]
    PaddedHourMinute = 1,
    #[serde(rename = "h:mm a")]
    HourMinuteAmPm = 2,
    #[serde(rename = "hh:mm a")]
    PaddedHourMinuteAmPm = 3,
    #[serde(rename = "h:mm:ss")]
    HourMinuteSecond = 4,
    #[serde(rename = "hh:mm:ss")]
    PaddedHourMinuteSecond = 5,
    #[serde(rename = "h:mm:ss a")]
    HourMinuteSecondAmPm = 6,
    #[serde(rename = "hh:mm:ss a")]
    PaddedHourMinuteSecondAmPm = 7,
    #[serde(rename = "h:mm:ss.zzz")]
    HourMinuteSecondMillis = 8,
    #[serde(rename = "hh:mm:ss.zzz")]
    PaddedHourMinuteSecondMillis = 9,
    #[serde(rename = "h:mm:ss.zzz a")]
    HourMinuteSecondMillisAmPm = 10,
    #[serde(rename = "hh:mm:ss.zzz a")]
    PaddedHourMinuteSecondMillisAmPm = 11,
}

/// Precision half of a legacy [`MessageTimestampFormat`], used only while
/// decoding one into the token model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageTimestampPrecision {
    Minutes,
    Seconds,
    Milliseconds,
}

impl MessageTimestampFormat {
    /// Compose the closest legacy discriminant from clock/padding/precision.
    ///
    /// Used only to project the live token pattern back onto the legacy enum
    /// for surfaces that still key caches on it.
    pub const fn from_parts(
        twelve_hour: bool,
        padded_hour: bool,
        precision: MessageTimestampPrecision,
    ) -> Self {
        match (twelve_hour, padded_hour, precision) {
            (false, false, MessageTimestampPrecision::Minutes) => Self::HourMinute,
            (false, true, MessageTimestampPrecision::Minutes) => Self::PaddedHourMinute,
            (true, false, MessageTimestampPrecision::Minutes) => Self::HourMinuteAmPm,
            (true, true, MessageTimestampPrecision::Minutes) => Self::PaddedHourMinuteAmPm,
            (false, false, MessageTimestampPrecision::Seconds) => Self::HourMinuteSecond,
            (false, true, MessageTimestampPrecision::Seconds) => Self::PaddedHourMinuteSecond,
            (true, false, MessageTimestampPrecision::Seconds) => Self::HourMinuteSecondAmPm,
            (true, true, MessageTimestampPrecision::Seconds) => Self::PaddedHourMinuteSecondAmPm,
            (false, false, MessageTimestampPrecision::Milliseconds) => Self::HourMinuteSecondMillis,
            (false, true, MessageTimestampPrecision::Milliseconds) => {
                Self::PaddedHourMinuteSecondMillis
            }
            (true, false, MessageTimestampPrecision::Milliseconds) => {
                Self::HourMinuteSecondMillisAmPm
            }
            (true, true, MessageTimestampPrecision::Milliseconds) => {
                Self::PaddedHourMinuteSecondMillisAmPm
            }
        }
    }

    /// Decode one legacy discriminant into the ordered token model.
    ///
    /// The mapping is written out for all twelve historical discriminants
    /// (`0`-`11`) on purpose: it must never depend on enum declaration order,
    /// `transmute`, or the arithmetic that the removed packed-atomic mirror
    /// used, because those would silently re-map every user's saved format if
    /// the variant list is ever reordered.
    pub fn into_pattern(self) -> TimestampPattern {
        let (twelve_hour, padded_hour, precision) = match self {
            // 0
            Self::HourMinute => (false, false, MessageTimestampPrecision::Minutes),
            // 1
            Self::PaddedHourMinute => (false, true, MessageTimestampPrecision::Minutes),
            // 2
            Self::HourMinuteAmPm => (true, false, MessageTimestampPrecision::Minutes),
            // 3
            Self::PaddedHourMinuteAmPm => (true, true, MessageTimestampPrecision::Minutes),
            // 4
            Self::HourMinuteSecond => (false, false, MessageTimestampPrecision::Seconds),
            // 5
            Self::PaddedHourMinuteSecond => (false, true, MessageTimestampPrecision::Seconds),
            // 6
            Self::HourMinuteSecondAmPm => (true, false, MessageTimestampPrecision::Seconds),
            // 7
            Self::PaddedHourMinuteSecondAmPm => (true, true, MessageTimestampPrecision::Seconds),
            // 8
            Self::HourMinuteSecondMillis => (false, false, MessageTimestampPrecision::Milliseconds),
            // 9
            Self::PaddedHourMinuteSecondMillis => {
                (false, true, MessageTimestampPrecision::Milliseconds)
            }
            // 10
            Self::HourMinuteSecondMillisAmPm => {
                (true, false, MessageTimestampPrecision::Milliseconds)
            }
            // 11
            Self::PaddedHourMinuteSecondMillisAmPm => {
                (true, true, MessageTimestampPrecision::Milliseconds)
            }
        };

        let mut tokens = Vec::with_capacity(9);
        tokens.push(if twelve_hour {
            TimestampToken::Hour12 {
                padded: padded_hour,
            }
        } else {
            TimestampToken::Hour24 {
                padded: padded_hour,
            }
        });
        tokens.push(TimestampToken::literal(":"));
        tokens.push(TimestampToken::Minute);
        if !matches!(precision, MessageTimestampPrecision::Minutes) {
            tokens.push(TimestampToken::literal(":"));
            tokens.push(TimestampToken::Second);
        }
        if matches!(precision, MessageTimestampPrecision::Milliseconds) {
            tokens.push(TimestampToken::literal("."));
            tokens.push(TimestampToken::Fraction { digits: 3 });
        }
        if twelve_hour {
            tokens.push(TimestampToken::literal(" "));
            tokens.push(TimestampToken::Meridiem { uppercase: false });
        }

        TimestampPattern {
            version: TIMESTAMP_PATTERN_VERSION,
            tokens,
        }
    }
}

// ============================================================================
// Message Timestamp Pattern ("puzzle piece" token model)
// ============================================================================

/// Serialized shape version of [`TimestampPattern`].
///
/// Bumping this is how a future token vocabulary declares that older Echo
/// builds must not silently reinterpret a pattern they cannot render.
pub const TIMESTAMP_PATTERN_VERSION: u8 = 1;

/// Hard ceiling on tokens in one pattern. Guards the settings builder, the
/// compiled op list, and the reserved canvas column against pathological input.
pub const MAX_TIMESTAMP_TOKENS: usize = 16;

/// Maximum visible characters in one custom literal token.
pub const MAX_TIMESTAMP_LITERAL_CHARS: usize = 8;

/// Literal separator presets offered by the settings builder, in menu order.
///
/// Deliberately plain ASCII: Echo has no authoritative locale-formatting
/// service, so it must not claim an "OS locale separator" token. Arbitrary
/// literals already cover worldwide arrangements without a new dependency.
pub const TIMESTAMP_LITERAL_PRESETS: &[(&str, &str)] = &[
    (":", "Colon"),
    (".", "Period"),
    (" ", "Space"),
    ("-", "Hyphen"),
    ("/", "Slash"),
    ("h", "Letter h"),
    ("m", "Letter m"),
    ("s", "Letter s"),
];

/// One ordered piece of a user-authored timestamp pattern.
///
/// Serialized internally tagged (`{"token":"minute"}`) so unknown future
/// tokens fail as a pattern-level decode error instead of silently changing
/// an existing token's meaning.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "token", rename_all = "snake_case")]
pub enum TimestampToken {
    Hour24 {
        #[serde(default)]
        padded: bool,
    },
    Hour12 {
        #[serde(default)]
        padded: bool,
    },
    Minute,
    Second,
    Fraction {
        digits: u8,
    },
    Meridiem {
        #[serde(default)]
        uppercase: bool,
    },
    Literal {
        text: String,
    },
}

/// Identity of a token independent of its properties.
///
/// Semantic time tokens are unique per pattern, so uniqueness, the mutually
/// exclusive clocks, and the add-menu enablement rules are all expressed
/// against this discriminant rather than against full token values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TimestampTokenKind {
    Hour24,
    Hour12,
    Minute,
    Second,
    Fraction,
    Meridiem,
    Literal,
}

impl TimestampTokenKind {
    /// Human label used by chips, the add menu, and reorder announcements.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Hour24 => "Hour (24h)",
            Self::Hour12 => "Hour (12h)",
            Self::Minute => "Minute",
            Self::Second => "Second",
            Self::Fraction => "Fraction",
            Self::Meridiem => "AM/PM",
            Self::Literal => "Separator",
        }
    }

    /// Whether this token contributes actual time information.
    pub const fn is_time_token(self) -> bool {
        !matches!(self, Self::Literal)
    }
}

impl TimestampToken {
    /// Build a literal token from a preset or custom string.
    pub fn literal(text: &str) -> Self {
        Self::Literal {
            text: text.to_string(),
        }
    }

    pub const fn kind(&self) -> TimestampTokenKind {
        match self {
            Self::Hour24 { .. } => TimestampTokenKind::Hour24,
            Self::Hour12 { .. } => TimestampTokenKind::Hour12,
            Self::Minute => TimestampTokenKind::Minute,
            Self::Second => TimestampTokenKind::Second,
            Self::Fraction { .. } => TimestampTokenKind::Fraction,
            Self::Meridiem { .. } => TimestampTokenKind::Meridiem,
            Self::Literal { .. } => TimestampTokenKind::Literal,
        }
    }

    /// Short chip caption, including the property that distinguishes variants.
    pub fn chip_label(&self) -> String {
        match self {
            Self::Hour24 { padded } => {
                if *padded {
                    "Hour 09".to_string()
                } else {
                    "Hour 9".to_string()
                }
            }
            Self::Hour12 { padded } => {
                if *padded {
                    "Hour 09 (12h)".to_string()
                } else {
                    "Hour 9 (12h)".to_string()
                }
            }
            Self::Minute => "Minute".to_string(),
            Self::Second => "Second".to_string(),
            Self::Fraction { digits } => format!("Fraction ({digits})"),
            Self::Meridiem { uppercase } => {
                if *uppercase {
                    "AM/PM".to_string()
                } else {
                    "am/pm".to_string()
                }
            }
            Self::Literal { text } => match text.as_str() {
                " " => "Space".to_string(),
                other => format!("\u{201c}{other}\u{201d}"),
            },
        }
    }

    /// Name used in accessible reorder labels ("Move Minute before Hour").
    pub fn accessible_name(&self) -> String {
        match self {
            Self::Literal { text } => match text.as_str() {
                " " => "space separator".to_string(),
                other => format!("separator {other}"),
            },
            other => other.kind().label().to_string(),
        }
    }

    /// Append this token's worst-case rendered text to a width probe.
    ///
    /// Values are the widest a token can ever render (`23`, `12`, `59`, `999`,
    /// `PM`), so a probe built in pattern order reserves a column that never
    /// clips regardless of the actual time being displayed.
    fn append_probe(&self, out: &mut String) {
        match self {
            Self::Hour24 { .. } => out.push_str("23"),
            Self::Hour12 { .. } => out.push_str("12"),
            Self::Minute | Self::Second => out.push_str("59"),
            Self::Fraction { digits } => {
                for _ in 0..(*digits).min(9) {
                    out.push('9');
                }
            }
            Self::Meridiem { uppercase } => out.push_str(if *uppercase { "PM" } else { "pm" }),
            Self::Literal { text } => out.push_str(text),
        }
    }
}

/// Why a pattern (or a proposed token addition) is not usable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimestampPatternError {
    UnsupportedVersion,
    NoTokens,
    NoTimeToken,
    TooManyTokens,
    MixedClocks,
    DuplicateTimeToken,
    MeridiemWithoutHour12,
    FractionWithoutSecond,
    InvalidFractionDigits,
    EmptyLiteral,
    LiteralTooLong,
    LiteralForbiddenCharacter,
}

impl TimestampPatternError {
    /// User-facing explanation, used verbatim by the settings builder for
    /// disabled add-menu entries.
    pub const fn message(self) -> &'static str {
        match self {
            Self::UnsupportedVersion => "This timestamp format was saved by a newer Echo version.",
            Self::NoTokens => "A timestamp format needs at least one piece.",
            Self::NoTimeToken => "Add at least one time piece, not only separators.",
            Self::TooManyTokens => "A timestamp format can hold at most 16 pieces.",
            Self::MixedClocks => "Use either the 24-hour or the 12-hour piece, not both.",
            Self::DuplicateTimeToken => "This piece is already in the format.",
            Self::MeridiemWithoutHour12 => "AM/PM needs the 12-hour piece.",
            Self::FractionWithoutSecond => "Fractions need the Second piece.",
            Self::InvalidFractionDigits => "Fractions support 1 to 3 digits.",
            Self::EmptyLiteral => "A separator cannot be empty.",
            Self::LiteralTooLong => "A separator can hold at most 8 characters.",
            Self::LiteralForbiddenCharacter => {
                "Separators cannot contain control or text-direction characters."
            }
        }
    }
}

/// Reject control and bidi/invisible characters so a literal can never reorder
/// or hide the rendered timestamp column.
fn timestamp_literal_char_is_allowed(ch: char) -> bool {
    if ch.is_control() {
        return false;
    }
    !matches!(
        ch,
        '\u{061C}'
            | '\u{200B}'..='\u{200F}'
            | '\u{202A}'..='\u{202E}'
            | '\u{2066}'..='\u{2069}'
            | '\u{FEFF}'
    )
}

/// Validate one custom literal before it becomes a token.
pub fn validate_timestamp_literal(text: &str) -> Result<(), TimestampPatternError> {
    let count = text.chars().count();
    if count == 0 {
        return Err(TimestampPatternError::EmptyLiteral);
    }
    if count > MAX_TIMESTAMP_LITERAL_CHARS {
        return Err(TimestampPatternError::LiteralTooLong);
    }
    if !text.chars().all(timestamp_literal_char_is_allowed) {
        return Err(TimestampPatternError::LiteralForbiddenCharacter);
    }
    Ok(())
}

/// A versioned, ordered user-authored timestamp format.
///
/// Any token order is valid; the constraints are semantic, not positional.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimestampPattern {
    pub version: u8,
    pub tokens: Vec<TimestampToken>,
}

impl Default for TimestampPattern {
    fn default() -> Self {
        Self {
            version: TIMESTAMP_PATTERN_VERSION,
            tokens: vec![
                TimestampToken::Hour24 { padded: false },
                TimestampToken::literal(":"),
                TimestampToken::Minute,
            ],
        }
    }
}

impl TimestampPattern {
    /// Validate the full pattern, including its serialized version.
    pub fn validate(&self) -> Result<(), TimestampPatternError> {
        if self.version == 0 || self.version > TIMESTAMP_PATTERN_VERSION {
            return Err(TimestampPatternError::UnsupportedVersion);
        }
        Self::validate_tokens(&self.tokens)
    }

    /// Validate a candidate token list independently of any stored version.
    pub fn validate_tokens(tokens: &[TimestampToken]) -> Result<(), TimestampPatternError> {
        if tokens.is_empty() {
            return Err(TimestampPatternError::NoTokens);
        }
        if tokens.len() > MAX_TIMESTAMP_TOKENS {
            return Err(TimestampPatternError::TooManyTokens);
        }

        let mut seen: Vec<TimestampTokenKind> = Vec::with_capacity(tokens.len());
        for token in tokens {
            let kind = token.kind();
            match token {
                TimestampToken::Fraction { digits } => {
                    if *digits == 0 || *digits > 3 {
                        return Err(TimestampPatternError::InvalidFractionDigits);
                    }
                }
                TimestampToken::Literal { text } => validate_timestamp_literal(text)?,
                _ => {}
            }
            // Literals may repeat; every semantic time token is unique.
            if kind.is_time_token() {
                if seen.contains(&kind) {
                    return Err(TimestampPatternError::DuplicateTimeToken);
                }
                seen.push(kind);
            }
        }

        if seen.is_empty() {
            return Err(TimestampPatternError::NoTimeToken);
        }
        if seen.contains(&TimestampTokenKind::Hour12) && seen.contains(&TimestampTokenKind::Hour24)
        {
            return Err(TimestampPatternError::MixedClocks);
        }
        if seen.contains(&TimestampTokenKind::Meridiem)
            && !seen.contains(&TimestampTokenKind::Hour12)
        {
            return Err(TimestampPatternError::MeridiemWithoutHour12);
        }
        if seen.contains(&TimestampTokenKind::Fraction)
            && !seen.contains(&TimestampTokenKind::Second)
        {
            return Err(TimestampPatternError::FractionWithoutSecond);
        }
        Ok(())
    }

    /// Whether the pattern already contains a token of this kind.
    pub fn contains_kind(&self, kind: TimestampTokenKind) -> bool {
        self.tokens.iter().any(|token| token.kind() == kind)
    }

    /// Whether appending `token` would keep the pattern valid, and why not.
    ///
    /// The settings builder disables an add-menu entry using exactly this
    /// result, so the menu can never offer an addition that would be rejected.
    pub fn can_append(&self, token: &TimestampToken) -> Result<(), TimestampPatternError> {
        let mut candidate = self.tokens.clone();
        candidate.push(token.clone());
        Self::validate_tokens(&candidate)
    }

    /// Finest time unit the pattern actually renders.
    pub fn resolution(&self) -> TimestampResolution {
        if self.contains_kind(TimestampTokenKind::Fraction) {
            TimestampResolution::Milliseconds
        } else if self.contains_kind(TimestampTokenKind::Second) {
            TimestampResolution::Seconds
        } else {
            TimestampResolution::Minutes
        }
    }

    /// Lower the validated pattern into an immutable execution plan.
    ///
    /// Compilation happens only when settings load or change; message
    /// formatting then executes the ops without parsing or locking settings.
    pub fn compile(&self, generation: u64) -> CompiledTimestampFormatter {
        let ops: Vec<TimestampFormatOp> = self
            .tokens
            .iter()
            .map(|token| match token {
                TimestampToken::Hour24 { padded } => TimestampFormatOp::Hour24 { padded: *padded },
                TimestampToken::Hour12 { padded } => TimestampFormatOp::Hour12 { padded: *padded },
                TimestampToken::Minute => TimestampFormatOp::Minute,
                TimestampToken::Second => TimestampFormatOp::Second,
                TimestampToken::Fraction { digits } => TimestampFormatOp::Fraction {
                    digits: (*digits).clamp(1, 3),
                },
                TimestampToken::Meridiem { uppercase } => TimestampFormatOp::Meridiem {
                    uppercase: *uppercase,
                },
                TimestampToken::Literal { text } => {
                    TimestampFormatOp::Literal(Arc::from(text.as_str()))
                }
            })
            .collect();

        let mut probe = String::with_capacity(16);
        for token in &self.tokens {
            token.append_probe(&mut probe);
        }

        CompiledTimestampFormatter {
            generation,
            ops: Arc::from(ops),
            resolution: self.resolution(),
            width_probe: Arc::from(probe.as_str()),
        }
    }
}

/// Finest unit a compiled pattern renders, and therefore the cache bucket a
/// formatted string stays valid for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TimestampResolution {
    Minutes,
    Seconds,
    Milliseconds,
}

impl TimestampResolution {
    /// Collapse an epoch-millisecond timestamp to the bucket in which the
    /// rendered string cannot change.
    #[inline]
    pub const fn bucket(self, timestamp_ms: i64) -> i64 {
        match self {
            Self::Minutes => timestamp_ms.div_euclid(60_000),
            Self::Seconds => timestamp_ms.div_euclid(1_000),
            Self::Milliseconds => timestamp_ms,
        }
    }

    /// Preview refresh interval in milliseconds for the settings builder.
    ///
    /// Milliseconds are capped at 10 Hz: a settings preview must never drive a
    /// per-frame wakeup.
    pub const fn preview_interval_ms(self) -> u64 {
        match self {
            Self::Minutes => 5_000,
            Self::Seconds => 1_000,
            Self::Milliseconds => 100,
        }
    }
}

/// One executable step of a compiled pattern.
#[derive(Debug, Clone)]
pub enum TimestampFormatOp {
    Hour24 { padded: bool },
    Hour12 { padded: bool },
    Minute,
    Second,
    Fraction { digits: u8 },
    Meridiem { uppercase: bool },
    Literal(Arc<str>),
}

/// Immutable, ready-to-execute timestamp formatter.
///
/// Published through `arc-swap`; message formatting loads one snapshot and runs
/// its ops. `generation` keys every derived cache (formatted strings, the
/// shaped width probe, retained canvas layouts) so a format change cannot serve
/// stale text or a stale reserved column.
#[derive(Debug)]
pub struct CompiledTimestampFormatter {
    pub generation: u64,
    pub ops: Arc<[TimestampFormatOp]>,
    pub resolution: TimestampResolution,
    pub width_probe: Arc<str>,
}

impl AsRef<CompiledTimestampFormatter> for CompiledTimestampFormatter {
    #[inline]
    fn as_ref(&self) -> &CompiledTimestampFormatter {
        self
    }
}

impl CompiledTimestampFormatter {
    /// Bucket an epoch-millisecond timestamp at this formatter's resolution.
    #[inline]
    pub const fn bucket(&self, timestamp_ms: i64) -> i64 {
        self.resolution.bucket(timestamp_ms)
    }

    /// Render local wall-clock components into `out`.
    ///
    /// Takes primitives rather than a `chrono` type so the compiled formatter
    /// stays a pure settings value with no calendar dependency.
    pub fn write_time(&self, hour24: u32, minute: u32, second: u32, millis: u32, out: &mut String) {
        use std::fmt::Write as _;

        for op in self.ops.iter() {
            match op {
                TimestampFormatOp::Hour24 { padded } => {
                    if *padded {
                        let _ = write!(out, "{hour24:02}");
                    } else {
                        let _ = write!(out, "{hour24}");
                    }
                }
                TimestampFormatOp::Hour12 { padded } => {
                    let hour = match hour24 % 12 {
                        0 => 12,
                        other => other,
                    };
                    if *padded {
                        let _ = write!(out, "{hour:02}");
                    } else {
                        let _ = write!(out, "{hour}");
                    }
                }
                TimestampFormatOp::Minute => {
                    let _ = write!(out, "{minute:02}");
                }
                TimestampFormatOp::Second => {
                    let _ = write!(out, "{second:02}");
                }
                TimestampFormatOp::Fraction { digits } => match digits {
                    1 => {
                        let _ = write!(out, "{}", millis / 100);
                    }
                    2 => {
                        let _ = write!(out, "{:02}", millis / 10);
                    }
                    _ => {
                        let _ = write!(out, "{millis:03}");
                    }
                },
                TimestampFormatOp::Meridiem { uppercase } => {
                    let text = match (hour24 < 12, *uppercase) {
                        (true, true) => "AM",
                        (true, false) => "am",
                        (false, true) => "PM",
                        (false, false) => "pm",
                    };
                    out.push_str(text);
                }
                TimestampFormatOp::Literal(text) => out.push_str(text),
            }
        }
    }

    /// Render local wall-clock components into a new string.
    pub fn format_time(&self, hour24: u32, minute: u32, second: u32, millis: u32) -> String {
        let mut out = String::with_capacity(self.width_probe.len());
        self.write_time(hour24, minute, second, millis, &mut out);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::ser::{Impossible, SerializeStruct, SerializeStructVariant, Serializer};
    use std::fmt;

    #[derive(Debug, PartialEq, Eq)]
    enum Shape {
        String(String),
        UnitVariant(String),
        Struct(Vec<(String, Shape)>),
    }

    #[derive(Debug)]
    struct ShapeError(String);

    impl fmt::Display for ShapeError {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str(&self.0)
        }
    }

    impl std::error::Error for ShapeError {}

    impl serde::ser::Error for ShapeError {
        fn custom<T: fmt::Display>(msg: T) -> Self {
            Self(msg.to_string())
        }
    }

    struct ShapeSerializer;

    struct ShapeStruct {
        fields: Vec<(String, Shape)>,
    }

    impl SerializeStruct for ShapeStruct {
        type Ok = Shape;
        type Error = ShapeError;

        fn serialize_field<T: ?Sized + Serialize>(
            &mut self,
            key: &'static str,
            value: &T,
        ) -> Result<(), Self::Error> {
            self.fields
                .push((key.to_string(), value.serialize(ShapeSerializer)?));
            Ok(())
        }

        fn end(self) -> Result<Self::Ok, Self::Error> {
            Ok(Shape::Struct(self.fields))
        }
    }

    impl SerializeStructVariant for ShapeStruct {
        type Ok = Shape;
        type Error = ShapeError;

        fn serialize_field<T: ?Sized + Serialize>(
            &mut self,
            key: &'static str,
            value: &T,
        ) -> Result<(), Self::Error> {
            SerializeStruct::serialize_field(self, key, value)
        }

        fn end(self) -> Result<Self::Ok, Self::Error> {
            SerializeStruct::end(self)
        }
    }

    impl Serializer for ShapeSerializer {
        type Ok = Shape;
        type Error = ShapeError;
        type SerializeSeq = Impossible<Shape, ShapeError>;
        type SerializeTuple = Impossible<Shape, ShapeError>;
        type SerializeTupleStruct = Impossible<Shape, ShapeError>;
        type SerializeTupleVariant = Impossible<Shape, ShapeError>;
        type SerializeMap = Impossible<Shape, ShapeError>;
        type SerializeStruct = ShapeStruct;
        type SerializeStructVariant = ShapeStruct;

        fn serialize_str(self, value: &str) -> Result<Self::Ok, Self::Error> {
            Ok(Shape::String(value.to_string()))
        }

        fn serialize_unit_variant(
            self,
            _name: &'static str,
            _variant_index: u32,
            variant: &'static str,
        ) -> Result<Self::Ok, Self::Error> {
            Ok(Shape::UnitVariant(variant.to_string()))
        }

        fn serialize_struct(
            self,
            _name: &'static str,
            len: usize,
        ) -> Result<Self::SerializeStruct, Self::Error> {
            Ok(ShapeStruct {
                fields: Vec::with_capacity(len),
            })
        }

        fn serialize_struct_variant(
            self,
            _name: &'static str,
            _variant_index: u32,
            _variant: &'static str,
            len: usize,
        ) -> Result<Self::SerializeStructVariant, Self::Error> {
            Ok(ShapeStruct {
                fields: Vec::with_capacity(len),
            })
        }

        fn serialize_bool(self, _value: bool) -> Result<Self::Ok, Self::Error> {
            unsupported()
        }
        fn serialize_i8(self, _value: i8) -> Result<Self::Ok, Self::Error> {
            unsupported()
        }
        fn serialize_i16(self, _value: i16) -> Result<Self::Ok, Self::Error> {
            unsupported()
        }
        fn serialize_i32(self, _value: i32) -> Result<Self::Ok, Self::Error> {
            unsupported()
        }
        fn serialize_i64(self, _value: i64) -> Result<Self::Ok, Self::Error> {
            unsupported()
        }
        fn serialize_u8(self, _value: u8) -> Result<Self::Ok, Self::Error> {
            unsupported()
        }
        fn serialize_u16(self, _value: u16) -> Result<Self::Ok, Self::Error> {
            unsupported()
        }
        fn serialize_u32(self, _value: u32) -> Result<Self::Ok, Self::Error> {
            unsupported()
        }
        fn serialize_u64(self, _value: u64) -> Result<Self::Ok, Self::Error> {
            unsupported()
        }
        fn serialize_f32(self, _value: f32) -> Result<Self::Ok, Self::Error> {
            unsupported()
        }
        fn serialize_f64(self, _value: f64) -> Result<Self::Ok, Self::Error> {
            unsupported()
        }
        fn serialize_char(self, _value: char) -> Result<Self::Ok, Self::Error> {
            unsupported()
        }
        fn serialize_bytes(self, _value: &[u8]) -> Result<Self::Ok, Self::Error> {
            unsupported()
        }
        fn serialize_none(self) -> Result<Self::Ok, Self::Error> {
            unsupported()
        }
        fn serialize_some<T: ?Sized + Serialize>(
            self,
            _value: &T,
        ) -> Result<Self::Ok, Self::Error> {
            unsupported()
        }
        fn serialize_unit(self) -> Result<Self::Ok, Self::Error> {
            unsupported()
        }
        fn serialize_unit_struct(self, _name: &'static str) -> Result<Self::Ok, Self::Error> {
            unsupported()
        }
        fn serialize_newtype_struct<T: ?Sized + Serialize>(
            self,
            _name: &'static str,
            _value: &T,
        ) -> Result<Self::Ok, Self::Error> {
            unsupported()
        }
        fn serialize_newtype_variant<T: ?Sized + Serialize>(
            self,
            _name: &'static str,
            _variant_index: u32,
            _variant: &'static str,
            _value: &T,
        ) -> Result<Self::Ok, Self::Error> {
            unsupported()
        }
        fn serialize_seq(self, _len: Option<usize>) -> Result<Self::SerializeSeq, Self::Error> {
            unsupported()
        }
        fn serialize_tuple(self, _len: usize) -> Result<Self::SerializeTuple, Self::Error> {
            unsupported()
        }
        fn serialize_tuple_struct(
            self,
            _name: &'static str,
            _len: usize,
        ) -> Result<Self::SerializeTupleStruct, Self::Error> {
            unsupported()
        }
        fn serialize_tuple_variant(
            self,
            _name: &'static str,
            _variant_index: u32,
            _variant: &'static str,
            _len: usize,
        ) -> Result<Self::SerializeTupleVariant, Self::Error> {
            unsupported()
        }
        fn serialize_map(self, _len: Option<usize>) -> Result<Self::SerializeMap, Self::Error> {
            unsupported()
        }
    }

    fn unsupported<T>() -> Result<T, ShapeError> {
        Err(ShapeError(
            "unsupported test serialization shape".to_string(),
        ))
    }

    #[test]
    fn legacy_patterns_map_to_valid_token_models() {
        let cases = [
            (
                MessageTimestampFormat::HourMinute,
                false,
                false,
                MessageTimestampPrecision::Minutes,
                "5:04",
            ),
            (
                MessageTimestampFormat::PaddedHourMinute,
                false,
                true,
                MessageTimestampPrecision::Minutes,
                "05:04",
            ),
            (
                MessageTimestampFormat::HourMinuteAmPm,
                true,
                false,
                MessageTimestampPrecision::Minutes,
                "5:04 am",
            ),
            (
                MessageTimestampFormat::PaddedHourMinuteAmPm,
                true,
                true,
                MessageTimestampPrecision::Minutes,
                "05:04 am",
            ),
            (
                MessageTimestampFormat::HourMinuteSecond,
                false,
                false,
                MessageTimestampPrecision::Seconds,
                "5:04:03",
            ),
            (
                MessageTimestampFormat::PaddedHourMinuteSecond,
                false,
                true,
                MessageTimestampPrecision::Seconds,
                "05:04:03",
            ),
            (
                MessageTimestampFormat::HourMinuteSecondAmPm,
                true,
                false,
                MessageTimestampPrecision::Seconds,
                "5:04:03 am",
            ),
            (
                MessageTimestampFormat::PaddedHourMinuteSecondAmPm,
                true,
                true,
                MessageTimestampPrecision::Seconds,
                "05:04:03 am",
            ),
            (
                MessageTimestampFormat::HourMinuteSecondMillis,
                false,
                false,
                MessageTimestampPrecision::Milliseconds,
                "5:04:03.021",
            ),
            (
                MessageTimestampFormat::PaddedHourMinuteSecondMillis,
                false,
                true,
                MessageTimestampPrecision::Milliseconds,
                "05:04:03.021",
            ),
            (
                MessageTimestampFormat::HourMinuteSecondMillisAmPm,
                true,
                false,
                MessageTimestampPrecision::Milliseconds,
                "5:04:03.021 am",
            ),
            (
                MessageTimestampFormat::PaddedHourMinuteSecondMillisAmPm,
                true,
                true,
                MessageTimestampPrecision::Milliseconds,
                "05:04:03.021 am",
            ),
        ];
        for (format, twelve_hour, padded, precision, expected) in cases {
            assert_eq!(
                MessageTimestampFormat::from_parts(twelve_hour, padded, precision),
                format
            );
            let pattern = format.into_pattern();
            assert_eq!(pattern.tokens, expected_legacy_tokens(format));
            assert_eq!(pattern.compile(1).format_time(5, 4, 3, 21), expected);
        }
    }

    fn expected_legacy_tokens(format: MessageTimestampFormat) -> Vec<TimestampToken> {
        use MessageTimestampFormat::*;
        let (twelve_hour, padded, seconds, millis) = match format {
            HourMinute => (false, false, false, false),
            PaddedHourMinute => (false, true, false, false),
            HourMinuteAmPm => (true, false, false, false),
            PaddedHourMinuteAmPm => (true, true, false, false),
            HourMinuteSecond => (false, false, true, false),
            PaddedHourMinuteSecond => (false, true, true, false),
            HourMinuteSecondAmPm => (true, false, true, false),
            PaddedHourMinuteSecondAmPm => (true, true, true, false),
            HourMinuteSecondMillis => (false, false, true, true),
            PaddedHourMinuteSecondMillis => (false, true, true, true),
            HourMinuteSecondMillisAmPm => (true, false, true, true),
            PaddedHourMinuteSecondMillisAmPm => (true, true, true, true),
        };
        let mut tokens = vec![
            if twelve_hour {
                TimestampToken::Hour12 { padded }
            } else {
                TimestampToken::Hour24 { padded }
            },
            TimestampToken::literal(":"),
            TimestampToken::Minute,
        ];
        if seconds {
            tokens.extend([TimestampToken::literal(":"), TimestampToken::Second]);
        }
        if millis {
            tokens.extend([
                TimestampToken::literal("."),
                TimestampToken::Fraction { digits: 3 },
            ]);
        }
        if twelve_hour {
            tokens.extend([
                TimestampToken::literal(" "),
                TimestampToken::Meridiem { uppercase: false },
            ]);
        }
        tokens
    }

    #[test]
    fn literal_validation_rejects_empty_long_control_and_bidi_text() {
        let invalid_patterns = [
            (
                TimestampPattern {
                    version: 0,
                    tokens: vec![TimestampToken::Minute],
                },
                TimestampPatternError::UnsupportedVersion,
            ),
            (
                TimestampPattern {
                    version: TIMESTAMP_PATTERN_VERSION,
                    tokens: vec![],
                },
                TimestampPatternError::NoTokens,
            ),
            (
                TimestampPattern {
                    version: TIMESTAMP_PATTERN_VERSION,
                    tokens: vec![TimestampToken::literal(":")],
                },
                TimestampPatternError::NoTimeToken,
            ),
            (
                TimestampPattern {
                    version: TIMESTAMP_PATTERN_VERSION,
                    tokens: vec![TimestampToken::Minute; MAX_TIMESTAMP_TOKENS + 1],
                },
                TimestampPatternError::TooManyTokens,
            ),
            (
                TimestampPattern {
                    version: TIMESTAMP_PATTERN_VERSION,
                    tokens: vec![
                        TimestampToken::Hour12 { padded: false },
                        TimestampToken::Hour24 { padded: false },
                    ],
                },
                TimestampPatternError::MixedClocks,
            ),
            (
                TimestampPattern {
                    version: TIMESTAMP_PATTERN_VERSION,
                    tokens: vec![TimestampToken::Minute, TimestampToken::Minute],
                },
                TimestampPatternError::DuplicateTimeToken,
            ),
            (
                TimestampPattern {
                    version: TIMESTAMP_PATTERN_VERSION,
                    tokens: vec![TimestampToken::Meridiem { uppercase: false }],
                },
                TimestampPatternError::MeridiemWithoutHour12,
            ),
            (
                TimestampPattern {
                    version: TIMESTAMP_PATTERN_VERSION,
                    tokens: vec![TimestampToken::Fraction { digits: 1 }],
                },
                TimestampPatternError::FractionWithoutSecond,
            ),
            (
                TimestampPattern {
                    version: TIMESTAMP_PATTERN_VERSION,
                    tokens: vec![
                        TimestampToken::Second,
                        TimestampToken::Fraction { digits: 0 },
                    ],
                },
                TimestampPatternError::InvalidFractionDigits,
            ),
            (
                TimestampPattern {
                    version: TIMESTAMP_PATTERN_VERSION,
                    tokens: vec![TimestampToken::Minute, TimestampToken::literal("")],
                },
                TimestampPatternError::EmptyLiteral,
            ),
            (
                TimestampPattern {
                    version: TIMESTAMP_PATTERN_VERSION,
                    tokens: vec![TimestampToken::Minute, TimestampToken::literal("123456789")],
                },
                TimestampPatternError::LiteralTooLong,
            ),
            (
                TimestampPattern {
                    version: TIMESTAMP_PATTERN_VERSION,
                    tokens: vec![TimestampToken::Minute, TimestampToken::literal("\u{202e}")],
                },
                TimestampPatternError::LiteralForbiddenCharacter,
            ),
        ];
        for (pattern, expected) in invalid_patterns {
            assert_eq!(pattern.validate(), Err(expected));
        }
        assert_eq!(
            validate_timestamp_literal(""),
            Err(TimestampPatternError::EmptyLiteral)
        );
        assert_eq!(
            validate_timestamp_literal("123456789"),
            Err(TimestampPatternError::LiteralTooLong)
        );
        assert_eq!(
            validate_timestamp_literal("\n"),
            Err(TimestampPatternError::LiteralForbiddenCharacter)
        );
        assert_eq!(
            validate_timestamp_literal("\u{202e}"),
            Err(TimestampPatternError::LiteralForbiddenCharacter)
        );
        assert_eq!(validate_timestamp_literal(" · "), Ok(()));
    }

    #[test]
    fn defaults_match_legacy_hour_minute() {
        assert_eq!(
            MessageTimestampFormat::default(),
            MessageTimestampFormat::HourMinute
        );
        assert_eq!(
            TimestampPattern::default(),
            MessageTimestampFormat::HourMinute.into_pattern()
        );
    }

    #[test]
    fn serde_shapes_preserve_legacy_names_and_tagged_minute_token() {
        let cases = [
            (MessageTimestampFormat::HourMinute, "h:mm"),
            (MessageTimestampFormat::PaddedHourMinute, "hh:mm"),
            (MessageTimestampFormat::HourMinuteAmPm, "h:mm a"),
            (MessageTimestampFormat::PaddedHourMinuteAmPm, "hh:mm a"),
            (MessageTimestampFormat::HourMinuteSecond, "h:mm:ss"),
            (MessageTimestampFormat::PaddedHourMinuteSecond, "hh:mm:ss"),
            (MessageTimestampFormat::HourMinuteSecondAmPm, "h:mm:ss a"),
            (
                MessageTimestampFormat::PaddedHourMinuteSecondAmPm,
                "hh:mm:ss a",
            ),
            (
                MessageTimestampFormat::HourMinuteSecondMillis,
                "h:mm:ss.zzz",
            ),
            (
                MessageTimestampFormat::PaddedHourMinuteSecondMillis,
                "hh:mm:ss.zzz",
            ),
            (
                MessageTimestampFormat::HourMinuteSecondMillisAmPm,
                "h:mm:ss.zzz a",
            ),
            (
                MessageTimestampFormat::PaddedHourMinuteSecondMillisAmPm,
                "hh:mm:ss.zzz a",
            ),
        ];
        for (format, expected) in cases {
            assert_eq!(
                format.serialize(ShapeSerializer).unwrap(),
                Shape::UnitVariant(expected.to_string())
            );
        }
        assert_eq!(
            TimestampToken::Minute.serialize(ShapeSerializer).unwrap(),
            Shape::Struct(vec![(
                "token".to_string(),
                Shape::String("minute".to_string())
            )])
        );
    }

    #[test]
    fn formatter_renders_24_hour_seconds_and_fraction() {
        let formatter = MessageTimestampFormat::PaddedHourMinuteSecondMillis
            .into_pattern()
            .compile(7);
        assert_eq!(formatter.format_time(5, 4, 3, 21), "05:04:03.021");
    }

    #[test]
    fn formatter_renders_12_hour_clock_and_meridiem() {
        let formatter = MessageTimestampFormat::PaddedHourMinuteAmPm
            .into_pattern()
            .compile(8);
        assert_eq!(formatter.format_time(0, 7, 0, 0), "12:07 am");
        assert_eq!(formatter.format_time(13, 7, 0, 0), "01:07 pm");
    }

    #[test]
    fn custom_token_order_is_preserved() {
        let pattern = TimestampPattern {
            version: TIMESTAMP_PATTERN_VERSION,
            tokens: vec![
                TimestampToken::Minute,
                TimestampToken::literal("/"),
                TimestampToken::Hour24 { padded: true },
            ],
        };
        assert_eq!(pattern.compile(9).format_time(6, 42, 0, 0), "42/06");
    }

    #[test]
    fn resolution_buckets_match_rendered_precision() {
        assert_eq!(TimestampResolution::Minutes.bucket(120_999), 2);
        assert_eq!(TimestampResolution::Seconds.bucket(120_999), 120);
        assert_eq!(TimestampResolution::Milliseconds.bucket(120_999), 120_999);
    }

    #[test]
    fn compiled_formatter_preserves_generation_and_width_probe() {
        let formatter = MessageTimestampFormat::HourMinuteSecondMillisAmPm
            .into_pattern()
            .compile(42);
        assert_eq!(formatter.generation, 42);
        assert_eq!(formatter.width_probe.as_ref(), "12:59:59.999 pm");
        assert_eq!(formatter.resolution, TimestampResolution::Milliseconds);
    }
}
