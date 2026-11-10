//! Compatibility facade for the chat render layout model.
//!
//! The geometry model, paint commands, and hitbox types live in
//! `echo-chat-render`. What stays here is the part that genuinely needs the
//! application shell: theme colors, user settings, and the GPUI font-metrics
//! provider used to reserve the timestamp column.

use gpui::{Hsla, Window};
use std::hash::{Hash, Hasher};
use std::sync::{LazyLock, Mutex, MutexGuard};

use crate::backend::settings;
use crate::theme::colors;
use crate::utils::parse_hex_color_opt;
use crate::utils::timestamp::format_message_timestamp_with_formatter;

pub use echo_chat_render::layout::*;

/// Trailing gutter added beside the measured timestamp text, scaled with the
/// chat font so the column never touches the following badge/username run.
const TIMESTAMP_COLUMN_PADDING_BASE: f32 = 3.0;

/// Last-resort per-character advance, used only when the shared font metrics
/// provider is unavailable (poisoned lock). Never used on the normal path.
const TIMESTAMP_FALLBACK_CHARACTER_WIDTH: f32 = 7.0;

/// Memoized timestamp column widths: `(probe key, width, exact)` slots, most
/// recent first, so windows on monitors with different scales don't evict
/// each other's measurement every frame.
///
/// The reserved column is measured from the compiled formatter's worst-case
/// probe (`23`, `12`, `59`, `59`, `999`, `PM` arranged in the user's token
/// order) instead of multiplying a character count by an estimated advance,
/// which silently under-reserved for wide separators and over-reserved for
/// digit-only patterns. Re-measuring happens once per formatter generation,
/// font scale, and device pixel ratio, not per message.
/// `(key, width, exact)` — `exact` marks a full `shape_line` measurement, which
/// wins over the glyph-advance estimate and is re-taken only when the key moves.
type TimestampWidthProbe = [(u64, f32, bool); 4];

const EMPTY_TIMESTAMP_WIDTH_PROBE: TimestampWidthProbe = [(0, 0.0, false); 4];

static TIMESTAMP_WIDTH_PROBE: LazyLock<Mutex<TimestampWidthProbe>> =
    LazyLock::new(|| Mutex::new(EMPTY_TIMESTAMP_WIDTH_PROBE));

/// Identity of one measured probe. `| 1` keeps it distinct from the initial
/// zero sentinel so a genuine hash of zero cannot read an unmeasured slot.
#[inline]
fn timestamp_probe_key(generation: u64, font_scale: f32, scale_factor: f32) -> u64 {
    let mut hasher = rustc_hash::FxHasher::default();
    generation.hash(&mut hasher);
    font_scale.to_bits().hash(&mut hasher);
    scale_factor.to_bits().hash(&mut hasher);
    hasher.finish() | 1
}

/// Measure the probe using GPUI-measured glyph advances for the chat font.
///
/// The shared provider is populated once at startup with real `shape_line`
/// measurements per glyph, so this is font-accurate rather than estimated.
fn measure_timestamp_probe(probe: &str, font_scale: f32) -> f32 {
    if let Some(provider) = super::font_metrics::calibrated_font_metrics() {
        return provider.measure_string(probe, false);
    }
    match super::font_metrics::FONT_METRICS_PROVIDER.read() {
        Ok(provider) => provider.measure_string(probe, false),
        Err(_) => probe.chars().count() as f32 * TIMESTAMP_FALLBACK_CHARACTER_WIDTH * font_scale,
    }
}

fn lock_timestamp_width_probe(
    probe: &Mutex<TimestampWidthProbe>,
) -> MutexGuard<'_, TimestampWidthProbe> {
    match probe.lock() {
        Ok(slot) => slot,
        Err(poisoned) => {
            let mut slot = poisoned.into_inner();
            *slot = EMPTY_TIMESTAMP_WIDTH_PROBE;
            probe.clear_poison();
            crate::log_ui!(warn, "Reset poisoned timestamp width probe cache");
            slot
        }
    }
}

fn cached_timestamp_probe(
    probe: &Mutex<TimestampWidthProbe>,
    key: u64,
    require_exact: bool,
) -> Option<f32> {
    let slots = lock_timestamp_width_probe(probe);
    slots
        .iter()
        .find(|slot| slot.0 == key)
        .filter(|slot| !require_exact || slot.2)
        .map(|slot| slot.1)
}

fn put_timestamp_probe(slots: &mut TimestampWidthProbe, key: u64, width: f32, exact: bool) {
    let index = slots
        .iter()
        .position(|slot| slot.0 == key)
        .unwrap_or(slots.len() - 1);
    slots[..=index].rotate_right(1);
    slots[0] = (key, width, exact);
}

/// Store a freshly measured probe width for `key`.
fn store_timestamp_probe(key: u64, width: f32, exact: bool) {
    put_timestamp_probe(
        &mut lock_timestamp_width_probe(&TIMESTAMP_WIDTH_PROBE),
        key,
        width,
        exact,
    );
}

/// Reserved width of the timestamp column for the active pattern.
#[inline]
pub fn timestamp_width() -> f32 {
    let formatter = settings::message_timestamp_formatter();
    let font_scale = settings::get_font_scale();
    let scale_factor = super::layout_scale::current();
    let key = timestamp_probe_key(formatter.generation, font_scale, scale_factor);

    let cached = cached_timestamp_probe(&TIMESTAMP_WIDTH_PROBE, key, false);
    if let Some(width) = cached {
        return width;
    }

    let width = measure_timestamp_probe(&formatter.width_probe, font_scale)
        + TIMESTAMP_COLUMN_PADDING_BASE * font_scale;
    store_timestamp_probe(key, width, false);
    width
}

/// Re-measure the timestamp column with full GPUI line shaping.
///
/// Identical inputs to [`timestamp_width`], but shapes the whole probe in one
/// pass so kerning and any ligature between separator glyphs is included. Call
/// it from a window-bearing layout path when exact reservation matters; the
/// result is stored under the same probe key, so [`timestamp_width`] returns it
/// until the formatter generation, font scale, or device pixel ratio changes.
pub fn refresh_timestamp_width_probe(window: &mut Window) -> f32 {
    let formatter = settings::message_timestamp_formatter();
    let font_scale = settings::get_font_scale();
    let scale_factor = super::layout_scale::current();
    let key = timestamp_probe_key(formatter.generation, font_scale, scale_factor);

    let exact_hit = cached_timestamp_probe(&TIMESTAMP_WIDTH_PROBE, key, true);
    if let Some(width) = exact_hit {
        return width;
    }

    let width = super::font_metrics::FONT_METRICS.measure_string_gpui(
        &formatter.width_probe,
        false,
        window,
    ) + TIMESTAMP_COLUMN_PADDING_BASE * font_scale;
    store_timestamp_probe(key, width, true);
    width
}

#[cfg(test)]
mod timestamp_width_probe_tests {
    use super::*;
    use core::prelude::v1::test;
    use std::panic::{AssertUnwindSafe, catch_unwind};

    #[test]
    fn cache_distinguishes_key_and_exact_measurement() {
        let probe = Mutex::new(EMPTY_TIMESTAMP_WIDTH_PROBE);
        put_timestamp_probe(&mut lock_timestamp_width_probe(&probe), 11, 42.0, false);

        assert_eq!(cached_timestamp_probe(&probe, 11, false), Some(42.0));
        assert_eq!(cached_timestamp_probe(&probe, 11, true), None);
        assert_eq!(cached_timestamp_probe(&probe, 12, false), None);

        put_timestamp_probe(&mut lock_timestamp_width_probe(&probe), 11, 43.0, true);
        assert_eq!(cached_timestamp_probe(&probe, 11, true), Some(43.0));
    }

    #[test]
    fn cache_keeps_recent_keys_for_several_scales() {
        let probe = Mutex::new(EMPTY_TIMESTAMP_WIDTH_PROBE);
        for (key, width) in [(11, 40.0), (13, 50.0), (15, 60.0), (17, 70.0)] {
            put_timestamp_probe(&mut lock_timestamp_width_probe(&probe), key, width, true);
        }
        assert_eq!(cached_timestamp_probe(&probe, 11, true), Some(40.0));
        assert_eq!(cached_timestamp_probe(&probe, 17, true), Some(70.0));

        put_timestamp_probe(&mut lock_timestamp_width_probe(&probe), 13, 51.0, true);
        put_timestamp_probe(&mut lock_timestamp_width_probe(&probe), 19, 80.0, true);
        assert_eq!(cached_timestamp_probe(&probe, 11, true), None);
        assert_eq!(cached_timestamp_probe(&probe, 13, true), Some(51.0));
        assert_eq!(cached_timestamp_probe(&probe, 19, true), Some(80.0));
    }

    #[test]
    fn poison_discards_partial_probe_and_allows_reuse() {
        let probe = Mutex::new(EMPTY_TIMESTAMP_WIDTH_PROBE);
        let _ = catch_unwind(AssertUnwindSafe(|| {
            let mut probe = probe.lock().expect("test probe should start healthy");
            probe[0] = (11, 13.0, true);
            panic!("poison test probe");
        }));

        assert!(probe.is_poisoned());
        assert_eq!(
            *lock_timestamp_width_probe(&probe),
            EMPTY_TIMESTAMP_WIDTH_PROBE
        );
        assert!(!probe.is_poisoned());

        put_timestamp_probe(&mut lock_timestamp_width_probe(&probe), 12, 21.0, true);
        assert_eq!(cached_timestamp_probe(&probe, 12, true), Some(21.0));
    }
}

/// Theme colors for a row's highlight treatment.
///
/// Colors resolve at paint time (rather than being baked into the layout) so a
/// theme or highlight-color change applies to already-built layouts.
pub trait HighlightColors {
    /// Get background color for this highlight type
    fn background_color(&self) -> Option<Hsla>;
    /// Get left border color for this highlight type
    fn border_color(&self) -> Option<Hsla>;
}

impl HighlightColors for HighlightType {
    fn background_color(&self) -> Option<Hsla> {
        match self {
            HighlightType::Whisper => Some(colors::whisper_bg()),
            HighlightType::Mention => Some(colors::mention_bg()),
            HighlightType::User(_) => Some(colors::accent_transparent(0.15)),
            HighlightType::ParticipatedReply(_) => Some(colors::thread_highlight_bg()),
            HighlightType::Badge(_) => None,
            HighlightType::HighlightedMessage => Some(colors::highlighted_message_bg()),
            HighlightType::FirstMessage => Some(colors::first_message_bg()),
            HighlightType::ReturningChatter => None,
            HighlightType::Subscription => Some(colors::subscription_bg()),
            HighlightType::Raid => Some(colors::raid_bg()),
            HighlightType::BitsBadge => Some(colors::bits_bg()),
            HighlightType::Announcement(color) => {
                Some(colors::announcement_bg_for(color.as_deref()))
            }
            HighlightType::WatchStreak => Some(colors::watchstreak_bg()),
            HighlightType::Phrase(_) => Some(colors::phrase_bg()),
            HighlightType::CustomPhrase(_, color)
            | HighlightType::CustomUser(_, color)
            | HighlightType::CustomBadge(_, color) => colors::custom_highlight_bg(color.as_deref()),
            HighlightType::SelfMessage => Some(colors::self_message_bg()),
            HighlightType::Elevated => Some(colors::elevated_bg()),
            HighlightType::ChannelPointRedeem { .. } => Some(colors::redeem_bg()),
        }
    }

    fn border_color(&self) -> Option<Hsla> {
        match self {
            HighlightType::Whisper => Some(colors::whisper_border()),
            HighlightType::Mention => Some(colors::mention_border()),
            HighlightType::User(_) => Some(colors::accent()),
            HighlightType::ParticipatedReply(_) => Some(colors::thread_highlight_border()),
            HighlightType::Badge(_) => None,
            HighlightType::HighlightedMessage => Some(colors::warning()),
            HighlightType::FirstMessage => Some(colors::first_message_border()),
            HighlightType::ReturningChatter => Some(colors::info()),
            HighlightType::Subscription => Some(colors::subscription_border()),
            HighlightType::Raid => Some(colors::twitch_purple()),
            HighlightType::BitsBadge => Some(colors::warning()),
            HighlightType::Announcement(_) => Some(colors::accent()),
            HighlightType::WatchStreak => Some(colors::watchstreak_border()),
            HighlightType::Phrase(_) => Some(colors::warning()),
            HighlightType::CustomPhrase(_, color)
            | HighlightType::CustomUser(_, color)
            | HighlightType::CustomBadge(_, color) => {
                colors::custom_highlight_border(color.as_deref())
            }
            HighlightType::SelfMessage => Some(colors::self_message_border()),
            HighlightType::Elevated => Some(colors::elevated_border()),
            HighlightType::ChannelPointRedeem { .. } => Some(colors::warning()),
        }
    }
}

/// Theme colors for a row's AutoMod / low-trust treatment.
pub trait LowTrustColors {
    /// Resolve `(background, left-border)` colors, or `None` for untreated rows.
    fn colors(self) -> Option<(Hsla, Hsla)>;
}

impl LowTrustColors for LowTrustTreatment {
    fn colors(self) -> Option<(Hsla, Hsla)> {
        match self {
            LowTrustTreatment::None => None,
            LowTrustTreatment::AutoMod => Some((colors::automod_bg(), colors::automod_border())),
            LowTrustTreatment::Monitored => {
                Some((colors::low_trust_bg(false), colors::low_trust_border(false)))
            }
            LowTrustTreatment::Restricted => {
                Some((colors::low_trust_bg(true), colors::low_trust_border(true)))
            }
        }
    }
}

use dashmap::DashMap;

/// Lock-free color cache for efficient color parsing.
/// PERF: Keyed on a fast FNV-1a hash of the hex string (u64) so cache hits do
/// zero allocation and zero mutex contention from background layout workers.
/// Twitch users have consistent colors, so this has ~95% hit rate.
static COLOR_CACHE: LazyLock<DashMap<u64, Hsla>> = LazyLock::new(|| DashMap::with_capacity(1000));

/// FNV-1a hash of a hex color string for use as the COLOR_CACHE key.
#[inline]
fn color_key(hex: &str) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in hex.as_bytes() {
        hash ^= b as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// Theme discriminant folded into the COLOR_CACHE key.
///
/// Cached values are theme-dependent on both paths -- readability
/// normalization measures contrast against the resolved chat background, and
/// the unparseable-hex fallback is a theme token -- so without this a theme
/// switch or custom-palette edit would keep serving colors computed for the
/// previous background until the process restarted.
#[inline]
fn theme_cache_token() -> u64 {
    let mode = match colors::current_theme() {
        colors::ThemeMode::Dark => 0u64,
        colors::ThemeMode::Light => 1,
        colors::ThemeMode::Midnight => 2,
        colors::ThemeMode::Custom => 3,
        colors::ThemeMode::ThemeFile => 4,
    };
    let (version, settings_version, _) = colors::render_key();
    ((version.rotate_left(32) ^ settings_version) << 4)
        | (mode << 1)
        | u64::from(colors::is_light_surface())
}

/// Get a cached parsed color, or parse and cache it
/// PERF: Lock-free DashMap lookup keyed on a hashed hex string (no per-miss
/// String allocation, no worker-thread mutex contention)
///
/// Every caller passes a Twitch-supplied user color, so the parsed value is
/// normalized for the active theme before it is cached.
pub fn get_cached_color(hex: &str) -> Hsla {
    let key = color_key(hex) ^ theme_cache_token().wrapping_mul(0x9E37_79B9_7F4A_7C15);
    if let Some(color) = COLOR_CACHE.get(&key) {
        return *color;
    }

    let color = parse_hex_color_opt(hex)
        .map(colors::normalize_user_color)
        .unwrap_or_else(colors::text_primary);
    // Distinct colors x theme switches grow without bound over a long
    // session; reset like TIMESTAMP_CACHE rather than track recency.
    if COLOR_CACHE.len() >= 8192 {
        COLOR_CACHE.clear();
    }
    COLOR_CACHE.insert(key, color);
    color
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct TimestampCacheKey {
    bucket: i64,
    /// Compiled-formatter generation. Folding it in means a pattern change can
    /// never serve a string rendered by the previous pattern, and the stale
    /// entries age out through the existing bound instead of needing a sweep.
    generation: u64,
}

/// Lock-free OS-local timestamp cache. Minute and second patterns share their
/// natural time bucket; millisecond patterns key the exact message timestamp.
/// The cache is bounded because high-volume millisecond formatting otherwise
/// produces one unique key per message.
static TIMESTAMP_CACHE: LazyLock<DashMap<TimestampCacheKey, String>> =
    LazyLock::new(|| DashMap::with_capacity(4096));

/// Get a cached OS-local timestamp using the active compiled formatter.
pub fn get_cached_timestamp(timestamp: i64) -> String {
    let formatter = settings::message_timestamp_formatter();
    let key = TimestampCacheKey {
        bucket: formatter.bucket(timestamp),
        generation: formatter.generation,
    };
    if let Some(s) = TIMESTAMP_CACHE.get(&key) {
        return s.clone();
    }
    let time_str = format_message_timestamp_with_formatter(timestamp, &formatter)
        .unwrap_or_else(|| "unknown".to_string());
    if TIMESTAMP_CACHE.len() >= 8192 {
        TIMESTAMP_CACHE.clear();
    }
    TIMESTAMP_CACHE.insert(key, time_str.clone());
    time_str
}
