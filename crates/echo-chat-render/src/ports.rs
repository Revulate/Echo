//! Root-installed ports for application state the render model needs.
//!
//! The layout model is intentionally free of settings and theme lookups so it
//! can be compiled and tested without the application shell. Echo installs the
//! real accessors once at startup; every port keeps a neutral default so the
//! crate's own unit tests observe the same geometry as Echo's default settings.

use gpui::Hsla;
use std::sync::OnceLock;

/// Looks up the decoded pixel size of an image URL, if it has been decoded.
pub type DecodedImageSizeFn = fn(&str) -> Option<(u32, u32)>;

static FONT_SCALE: OnceLock<fn() -> f32> = OnceLock::new();
static CHAT_DENSITY_MULTIPLIER: OnceLock<fn() -> f32> = OnceLock::new();
static BADGE_SCALE: OnceLock<fn() -> f32> = OnceLock::new();
static EMOTE_SCALE: OnceLock<fn() -> f32> = OnceLock::new();
static CLIPBOARD_WRITER: OnceLock<fn(&str)> = OnceLock::new();
static TEXT_PRIMARY: OnceLock<fn() -> Hsla> = OnceLock::new();
static DECODED_IMAGE_SIZE: OnceLock<DecodedImageSizeFn> = OnceLock::new();
static BADGE_SET_PENDING: OnceLock<fn(&str) -> bool> = OnceLock::new();

/// Install the user's chat font scale accessor.
pub fn install_font_scale(callback: fn() -> f32) {
    let _ = FONT_SCALE.set(callback);
}

/// Install the active chat density multiplier accessor.
pub fn install_chat_density_multiplier(callback: fn() -> f32) {
    let _ = CHAT_DENSITY_MULTIPLIER.set(callback);
}

/// Install the user's badge scale accessor.
pub fn install_badge_scale(callback: fn() -> f32) {
    let _ = BADGE_SCALE.set(callback);
}

/// Install the user's emote scale accessor.
pub fn install_emote_scale(callback: fn() -> f32) {
    let _ = EMOTE_SCALE.set(callback);
}

/// Install the system clipboard writer used by selection copy.
pub fn install_clipboard_writer(callback: fn(&str)) {
    let _ = CLIPBOARD_WRITER.set(callback);
}

/// Install the theme's primary text color accessor.
pub fn install_text_primary(callback: fn() -> Hsla) {
    let _ = TEXT_PRIMARY.set(callback);
}

/// Install the decoded image-size lookup used to refine native emote geometry.
///
/// The root owns cache residency and animation-frame policy; the render model
/// only receives an already-decoded, side-effect-free size probe.
pub fn install_decoded_image_size(callback: DecodedImageSizeFn) {
    let _ = DECODED_IMAGE_SIZE.set(callback);
}

/// Install the probe that reports whether a channel's (or the global) Twitch
/// badge set is still loading, so layouts can reserve slots for IRC badges.
pub fn install_badge_set_pending(callback: fn(&str) -> bool) {
    let _ = BADGE_SET_PENDING.set(callback);
}

/// Current chat font scale. Defaults to Echo's `font_scale` default of 1.0.
#[inline]
pub fn font_scale() -> f32 {
    FONT_SCALE.get().map_or(1.0, |callback| callback())
}

/// Current chat density multiplier. Defaults to `ChatDensity::Normal` (1.0).
#[inline]
pub fn chat_density_multiplier() -> f32 {
    CHAT_DENSITY_MULTIPLIER
        .get()
        .map_or(1.0, |callback| callback())
}

/// Current badge scale. Defaults to Echo's `badge_scale` default of 1.0.
#[inline]
pub fn badge_scale() -> f32 {
    BADGE_SCALE.get().map_or(1.0, |callback| callback())
}

/// Current emote scale. Defaults to Echo's `emote_scale` default of 1.0.
#[inline]
pub fn emote_scale() -> f32 {
    EMOTE_SCALE.get().map_or(1.0, |callback| callback())
}

/// Write `text` to the system clipboard. Defaults to discarding the text, so a
/// standalone test or contract tool never touches the developer's clipboard.
#[inline]
pub fn copy_text(text: &str) {
    if let Some(callback) = CLIPBOARD_WRITER.get() {
        callback(text);
    }
}

/// The theme's primary text color, used for the measurement text runs handed to
/// GPUI's shaper. Glyph color does not affect advance widths, so the default is
/// an opaque white rather than a theme lookup.
#[inline]
pub fn text_primary() -> Hsla {
    TEXT_PRIMARY
        .get()
        .map_or_else(gpui::white, |callback| callback())
}

/// Whether the Twitch badge metadata a row in `channel` needs is still loading.
/// Defaults to false: nothing is reserved until the application installs a probe.
#[inline]
pub fn badge_set_pending(channel: &str) -> bool {
    BADGE_SET_PENDING
        .get()
        .is_some_and(|callback| callback(channel))
}

/// Return decoded pixel dimensions already resident in the application's media
/// cache. The neutral default keeps standalone layout tests deterministic.
#[inline]
pub fn decoded_image_size(url: &str) -> Option<(u32, u32)> {
    DECODED_IMAGE_SIZE.get().and_then(|callback| callback(url))
}
