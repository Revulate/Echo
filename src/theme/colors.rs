//! Color palette for Echo GPUI
//!
//! Based on the Flutter app's color system.
//! Uses GPUI's Hsla color type.
//! Colors are now dynamic - they read from settings.

use crate::backend::settings;
use gpui::{Hsla, Rgba};
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};

/// Global version counter - incremented when theme changes
static THEME_VERSION: AtomicU64 = AtomicU64::new(0);

/// Identity of the resolved theme: this module's invalidation counter plus the
/// settings-side theme version. Both are atomics, so a cache hit never touches
/// the settings mutex, and a change made anywhere (OS theme flip, palette edit,
/// settings import) is visible before the polling coordinator drains its event.
type ThemeCacheKey = (u64, u64);

#[inline]
fn theme_cache_key() -> ThemeCacheKey {
    (
        THEME_VERSION.load(Ordering::Acquire),
        settings::get_theme_settings_version(),
    )
}

thread_local! {
    /// Thread-local cache: (cache key, theme_mode)
    static THEME_CACHE: Cell<Option<(ThemeCacheKey, ThemeMode)>> = const { Cell::new(None) };
    /// Thread-local primary-text override parsed once per theme version.
    static PRIMARY_TEXT_OVERRIDE_CACHE: Cell<Option<(ThemeCacheKey, Option<Hsla>)>> =
        const { Cell::new(None) };
    /// Thread-local custom/theme-file palette snapshot, so custom themes do not
    /// clone the whole palette out of settings for every color lookup.
    static PALETTE_CACHE: RefCell<Option<(ThemeCacheKey, Rc<settings::GeneratedPalette>)>> =
        const { RefCell::new(None) };
    static SWITCH_STYLE_CACHE: Cell<Option<(ThemeCacheKey, settings::SwitchOnStyle)>> =
        const { Cell::new(None) };
}

pub fn settings_switch_uses_accent() -> bool {
    let key = theme_cache_key();
    let style = SWITCH_STYLE_CACHE.with(|cache| {
        if let Some((cached_key, style)) = cache.get()
            && cached_key == key
        {
            return style;
        }
        let style = settings::get_switch_on_style();
        cache.set(Some((key, style)));
        style
    });
    style == settings::SwitchOnStyle::Accent
}

/// Identity of every input the palette resolves from, for retained UI that
/// holds painted colors and must rebuild when any of them changes.
pub fn render_key() -> (u64, u64, bool) {
    let (version, settings_version) = theme_cache_key();
    (
        version,
        settings_version,
        settings::get_window_transparent_cached(),
    )
}

/// Call this when theme settings change to invalidate the cache
pub fn invalidate_theme_cache() {
    THEME_VERSION.fetch_add(1, Ordering::Release);
}

/// Get cached theme mode, recomputing only when the cache key changes
fn cached_theme() -> ThemeMode {
    let key = theme_cache_key();
    THEME_CACHE.with(|cache| {
        if let Some((cached_key, mode)) = cache.get()
            && cached_key == key
        {
            return mode;
        }

        let mode = compute_theme_mode();
        cache.set(Some((key, mode)));
        mode
    })
}

/// Convert a hex color (#AARRGGBB, #RRGGBB, or #RGB) to GPUI's Hsla.
///
/// Any alpha channel in an `#AARRGGBB` value is ignored here (result is opaque);
/// use [`hex_to_hsla_argb`] when the stored alpha should be honored.
pub fn hex_to_hsla(hex: &str) -> Option<Hsla> {
    let (r, g, b) = settings::hex_to_rgb(hex)?;
    Some(rgb_to_hsla(r, g, b, 1.0))
}

/// Convert an `#AARRGGBB`/`#RRGGBB`/`#RGB` color to Hsla, honoring the alpha
/// channel when present and falling back to `default_alpha` otherwise.
///
/// Each highlight type has a default alpha (e.g. self-mention 127/255) that
/// applies unless the user has stored an explicit 8-digit color.
pub fn hex_to_hsla_argb(hex: &str, default_alpha: f32) -> Option<Hsla> {
    let (r, g, b, a) = settings::hex_to_rgba(hex)?;
    let alpha = a.map(|a| a as f32 / 255.0).unwrap_or(default_alpha);
    Some(rgb_to_hsla(r, g, b, alpha))
}

/// Convert 8-bit RGB channels plus a normalized alpha into GPUI Hsla.
fn rgb_to_hsla(r: u8, g: u8, b: u8, a: f32) -> Hsla {
    let (h, s, l) = crate::backend::settings::rgb_to_hsl(r, g, b);
    Hsla { h, s, l, a }
}

/// Per-highlight-type default background alphas (rgba alpha / 255).
///
/// Used when the stored highlight color has no explicit alpha channel.
pub mod highlight_alpha {
    /// Self-mention / phrase-fallback / whisper: rgba(_, _, _, 127).
    pub const MENTION: f32 = 127.0 / 255.0;
    pub const WHISPER: f32 = 127.0 / 255.0;
    pub const PHRASE: f32 = 127.0 / 255.0;
    /// Self-message: rgba(0,118,221,115).
    pub const SELF_MESSAGE: f32 = 115.0 / 255.0;
    /// Redeemed: rgba(28,126,141,60).
    pub const REDEEMED: f32 = 60.0 / 255.0;
    /// First message: rgba(72,127,63,60).
    pub const FIRST_MESSAGE: f32 = 60.0 / 255.0;
    /// Elevated / HypeChat: rgba(255,174,66,60).
    pub const ELEVATED: f32 = 60.0 / 255.0;
    /// Reply thread: rgba(143,48,24,60).
    pub const REPLY_THREAD: f32 = 60.0 / 255.0;
    /// Subscription: rgba(196,102,255,100).
    pub const SUBSCRIPTION: f32 = 100.0 / 255.0;
    /// Watch streak: rgba(0,130,255,70).
    pub const WATCH_STREAK: f32 = 70.0 / 255.0;
    /// Announcement: rgba(255,102,237,100).
    pub const ANNOUNCEMENT: f32 = 100.0 / 255.0;
    /// Custom user/badge rules and the generic highlighted-message tint.
    pub const HIGHLIGHTED: f32 = 100.0 / 255.0;
    /// AutoMod-held row wash: grey applied strongly enough to read as "held"
    /// while keeping the caught text legible.
    pub const AUTOMOD: f32 = 115.0 / 255.0;
}

/// Lighten an Hsla color by increasing lightness
fn lighten_hsla(color: Hsla, amount: f32) -> Hsla {
    Hsla {
        h: color.h,
        s: color.s,
        l: (color.l + amount).min(1.0),
        a: color.a,
    }
}

/// Current theme mode
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ThemeMode {
    Dark,
    Light,
    Midnight,
    Custom,
    ThemeFile,
}

/// Get current theme mode (cached - fast path for render loops)
pub fn current_theme() -> ThemeMode {
    cached_theme()
}

fn compute_theme_mode() -> ThemeMode {
    match settings::get_theme().as_deref() {
        Some(t) if t.eq_ignore_ascii_case("light") => ThemeMode::Light,
        Some(t) if t.eq_ignore_ascii_case("system") => system_theme_mode(),
        Some(t) if t.eq_ignore_ascii_case("midnight") => ThemeMode::Midnight,
        Some(t) if t.eq_ignore_ascii_case("custom") => ThemeMode::Custom,
        Some(t) if t.eq_ignore_ascii_case("theme file") => ThemeMode::ThemeFile,
        _ => ThemeMode::Dark,
    }
}

fn system_theme_mode() -> ThemeMode {
    if settings::get_resolved_system_theme_prefers_light().unwrap_or(false) {
        ThemeMode::Light
    } else {
        ThemeMode::Dark
    }
}

/// True when chat is painted on a light background.
///
/// Derived from the resolved surface color rather than the [`ThemeMode`]
/// discriminant, so a custom palette or theme file that happens to be light
/// classifies correctly instead of falling into the dark branch.
pub fn is_light_surface() -> bool {
    bg_surface().l > 0.5
}

/// Pull a Twitch-supplied user color into a readable range for the active
/// theme.
///
/// Twitch user colors are mostly chosen for dark backgrounds, so on a light
/// theme the pale entries render as near-invisible text (and the
/// similar-message dim makes them worse); on a dark theme the very dark
/// entries, such as navy, do the same. The color is measured against the chat
/// background ([`bg_void`], which the message list paints) and only its
/// lightness changes; see `readable_user_color`.
pub fn normalize_user_color(color: Hsla) -> Hsla {
    readable_user_color(color, bg_void())
}

/// Adjust `color`'s HSL lightness until its WCAG contrast against `surface`
/// reaches [`CONTRAST_TEXT_MIN`].
///
/// A username is drawn at the chat body size, below WCAG's large-text
/// threshold, and is the only label on the speaker, so it gets the AA
/// normal-text floor rather than the 3:1 large-text one.
///
/// - Colors that already meet the target are returned unchanged.
/// - Hue, saturation and alpha are always kept, so a name keeps its identity.
/// - Lightness moves toward black when black reads better on `surface` than
///   white does, and toward white otherwise.
/// - The target is always reachable: one of black and white has at least
///   sqrt(21) ~ 4.58:1 against any surface. At fixed hue and saturation
///   luminance rises monotonically with HSL lightness, so the span between the
///   original lightness and that extreme has a single pass/fail boundary, and a
///   binary search returns the readable lightness closest to the original in
///   that direction, which is not necessarily the closest one overall.
/// - Alpha on either color is ignored, like [`contrast_ratio`]; a translucent
///   window background is measured as its opaque theme color.
fn readable_user_color(color: Hsla, surface: Hsla) -> Hsla {
    if contrast_ratio(color, surface) >= CONTRAST_TEXT_MIN {
        return color;
    }
    let mut readable = text_on_aa(surface).l;
    let mut unreadable = color.l;
    // 16 halvings resolve lightness to 1/65536, far below one 8-bit step.
    for _ in 0..16 {
        let l = (readable + unreadable) * 0.5;
        if contrast_ratio(Hsla { l, ..color }, surface) >= CONTRAST_TEXT_MIN {
            readable = l;
        } else {
            unreadable = l;
        }
    }
    Hsla {
        l: readable,
        ..color
    }
}

/// Get the generated palette for the resolved custom theme, cached per theme
/// cache key so a palette is generated (and cloned out of settings) at most
/// once per theme change instead of once per color lookup.
fn cached_palette() -> Rc<settings::GeneratedPalette> {
    let key = theme_cache_key();
    PALETTE_CACHE.with(|cache| {
        {
            let cached = cache.borrow();
            if let Some((cached_key, palette)) = &*cached
                && *cached_key == key
            {
                return Rc::clone(palette);
            }
        }

        let palette = Rc::new(if current_theme() == ThemeMode::ThemeFile {
            settings::get_custom_theme_file_palette().unwrap_or_else(settings::get_custom_palette)
        } else {
            settings::get_custom_palette()
        });
        *cache.borrow_mut() = Some((key, Rc::clone(&palette)));
        palette
    })
}

/// Get a color from the generated custom theme palette
fn custom_color(field: &str, fallback: Hsla) -> Hsla {
    let palette = cached_palette();
    let hex = match field {
        "bg_void" => &palette.bg_void,
        "bg_surface" => &palette.bg_surface,
        "bg_plate" => &palette.bg_plate,
        "bg_elevated" => &palette.bg_elevated,
        "bg_hover" => &palette.bg_hover,
        "text_primary" => &palette.text_primary,
        "text_secondary" => &palette.text_secondary,
        "text_muted" => &palette.text_muted,
        "border" => &palette.border,
        "border_strong" => &palette.border_strong,
        _ => return fallback,
    };
    hex_to_hsla(hex).unwrap_or(fallback)
}

/// Dispatch color based on current theme.
/// Reduces boilerplate in individual color functions.
/// For colors that support custom theme, pass the custom_field name.
#[inline]
fn theme_color(dark: Hsla, light: Hsla, midnight: Hsla, custom_field: Option<&str>) -> Hsla {
    match current_theme() {
        ThemeMode::Dark => dark,
        ThemeMode::Light => light,
        ThemeMode::Midnight => midnight,
        ThemeMode::Custom | ThemeMode::ThemeFile => match custom_field {
            Some(field) => custom_color(field, dark),
            None => dark,
        },
    }
}

thread_local! {
    /// The native window whose element tree is currently being built.
    ///
    /// GPUI builds and paints one window at a time on the UI thread. Keeping
    /// this role alive for the full frame lets retained chat-paint callbacks
    /// share the main window's background policy without leaking translucent
    /// theme fills into explicitly opaque native popouts.
    static MAIN_WINDOW_FRAME: Cell<bool> = const { Cell::new(false) };
}

/// Enter the main native window's render frame.
pub(crate) fn begin_main_window_frame() {
    MAIN_WINDOW_FRAME.with(|active| active.set(true));
}

/// Enter a secondary native window's render frame.
pub(crate) fn begin_secondary_window_frame() {
    MAIN_WINDOW_FRAME.with(|active| active.set(false));
}

/// Whether GPUI is currently building or painting the main native window.
///
/// The marker deliberately spans the full window frame so low-level retained
/// paint callbacks can keep process-global diagnostics scoped to the window
/// that owns Echo's metrics overlay.
pub(crate) fn main_window_frame_active() -> bool {
    MAIN_WINDOW_FRAME.with(Cell::get)
}

fn apply_window_background_transparency(color: Hsla, transparent_alpha: f32) -> Hsla {
    let main_window_frame = main_window_frame_active();
    if main_window_frame && settings::get_window_transparent_cached() {
        color.opacity(transparent_alpha)
    } else {
        color
    }
}

/// Variant for colors where Dark and Midnight share the same value
#[inline]
fn theme_color_dark_midnight(dark_midnight: Hsla, light: Hsla, custom_field: Option<&str>) -> Hsla {
    theme_color(dark_midnight, light, dark_midnight, custom_field)
}

/// Void background (darkest) - behind everything
pub fn bg_void() -> Hsla {
    apply_window_background_transparency(
        theme_color(
            hsla(0.0, 0.0, 0.02, 1.0),
            hsla(0.0, 0.0, 0.96, 1.0),
            hsla(240.0 / 360.0, 0.15, 0.01, 1.0),
            Some("bg_void"),
        ),
        0.28,
    )
}

/// Surface background - main content areas
pub fn bg_surface() -> Hsla {
    apply_window_background_transparency(
        theme_color(
            hsla(0.0, 0.0, 0.06, 1.0),
            hsla(0.0, 0.0, 1.0, 1.0),
            hsla(240.0 / 360.0, 0.12, 0.05, 1.0),
            Some("bg_surface"),
        ),
        0.84,
    )
}

/// Neutral gray focus surface for the active split header.
///
/// Dark surfaces are lifted; already-light surfaces are shaded by the same
/// amount because pure white cannot be lightened. The hue is dropped so tinted
/// custom palettes get a plain gray step, like the zebra row overlay; on the
/// dark surfaces this lands near #1a1a1a.
/// Window transparency is kept from the resolved surface.
pub fn active_split_header_bg() -> Hsla {
    let surface = bg_surface();
    let lightness = if surface.l >= 0.5 {
        (surface.l - 0.05).max(0.0)
    } else {
        (surface.l + 0.05).min(1.0)
    };
    hsla(0.0, 0.0, lightness, surface.a)
}

/// Quiet Settings-only group fill.
///
/// Light settings popouts otherwise place white groups on a white page. A
/// small cool-gray wash gives groups an edge without changing chat, overlay,
/// or ordinary application surfaces. Dark themes deliberately retain their
/// existing surface color.
pub fn settings_group_surface() -> Hsla {
    let surface = bg_surface();
    if is_light_surface() {
        opaque_composite(surface, hsla(218.0 / 360.0, 0.25, 0.65, 0.06))
    } else {
        surface
    }
}

/// Hairline color for [`settings_group_surface`].
///
/// The caller may paint this unconditionally: on dark themes it is the group
/// surface itself, preserving the previous no-border treatment.
pub fn settings_group_border(surface: Hsla) -> Hsla {
    if is_light_surface() {
        contrast_token(
            ContrastRole::GroupBorder,
            surface,
            border_subtle(),
            CONTRAST_SUBTLE_RULE_MIN,
            qualify_by_lightness,
        )
    } else {
        surface
    }
}

fn settings_switch_off_track(light: bool, enabled: bool) -> Hsla {
    match (light, enabled) {
        // Both light values clear 3:1 against the quiet light group fill while
        // remaining visibly secondary to the on state.
        (true, true) => Hsla::from(Rgba {
            r: 102.0 / 255.0,
            g: 112.0 / 255.0,
            b: 133.0 / 255.0,
            a: 1.0,
        }),
        (true, false) => Hsla::from(Rgba {
            r: 123.0 / 255.0,
            g: 132.0 / 255.0,
            b: 148.0 / 255.0,
            a: 1.0,
        }),
        // Dark keeps its existing low-key appearance. The disabled value is
        // intentionally dimmer than the enabled-off track while still
        // clearing the non-text threshold against the dark group surface.
        (false, true) => Hsla::from(Rgba {
            r: 107.0 / 255.0,
            g: 114.0 / 255.0,
            b: 128.0 / 255.0,
            a: 1.0,
        }),
        (false, false) => Hsla::from(Rgba {
            r: 89.0 / 255.0,
            g: 98.0 / 255.0,
            b: 115.0 / 255.0,
            a: 1.0,
        }),
    }
}

fn settings_switch_on_track(light: bool, enabled: bool) -> Hsla {
    // Keep state quiet and neutral. Accent marks selection, keyboard focus,
    // and primary actions; it is not the settings on-state.
    let lightness = match (light, enabled) {
        (true, true) => 0.22,
        (true, false) => 0.32,
        (false, true) => 0.88,
        (false, false) => 0.68,
    };
    hsla(0.0, 0.0, lightness, 1.0)
}

/// Settings switch fill with contrast-qualified neutral state tokens.
///
/// Callers keep disabled controls opaque; dimming a complete switch with an
/// element opacity would undo this token's 3:1 guarantee.
pub fn settings_switch_track(is_on: bool, enabled: bool) -> Hsla {
    let style = if settings_switch_uses_accent() {
        settings::SwitchOnStyle::Accent
    } else {
        settings::SwitchOnStyle::Neutral
    };
    resolve_switch_track(
        style,
        is_on,
        enabled,
        settings_group_surface(),
        if is_on && style == settings::SwitchOnStyle::Accent {
            switch_accent()
        } else {
            DEFAULT_ACCENT
        },
    )
}

fn resolve_switch_track(
    style: settings::SwitchOnStyle,
    is_on: bool,
    enabled: bool,
    surface: Hsla,
    accent: Hsla,
) -> Hsla {
    if is_on && style == settings::SwitchOnStyle::Accent {
        contrast_token(
            if enabled {
                ContrastRole::SwitchAccentOnTrack
            } else {
                ContrastRole::SwitchAccentDisabledOnTrack
            },
            surface,
            accent,
            CONTRAST_NON_TEXT_MIN,
            if enabled {
                qualify_accent_switch_track
            } else {
                qualify_disabled_accent_switch_track
            },
        )
    } else if is_on {
        contrast_token(
            if enabled {
                ContrastRole::SwitchOnTrack
            } else {
                ContrastRole::SwitchDisabledOnTrack
            },
            surface,
            settings_switch_on_track(surface.l > 0.5, enabled),
            CONTRAST_NON_TEXT_MIN,
            qualify_switch_track,
        )
    } else {
        contrast_token(
            if enabled {
                ContrastRole::SwitchOffTrack
            } else {
                ContrastRole::SwitchDisabledTrack
            },
            surface,
            settings_switch_off_track(surface.l > 0.5, enabled),
            CONTRAST_NON_TEXT_MIN,
            qualify_switch_track,
        )
    }
}

/// Switches honor stored ARGB alpha without changing other accent consumers.
fn switch_accent() -> Hsla {
    if current_theme() == ThemeMode::ThemeFile
        && let Some(hex) = settings::get_custom_theme_file_accent()
        && let Some(color) = hex_to_hsla_argb(&hex, 1.0)
    {
        return color;
    }
    hex_to_hsla_argb(&settings::get_primary_accent_color(), 1.0).unwrap_or(DEFAULT_ACCENT)
}

/// Contrasting opaque knob for a settings switch.
pub fn settings_switch_knob(is_on: bool, enabled: bool) -> Hsla {
    text_on(settings_switch_track(is_on, enabled))
}

/// Channel-name color for the active split: the accent, darkened or lightened
/// until it reads as text on `surface`.
pub fn active_split_label(surface: Hsla) -> Hsla {
    contrast_token(
        ContrastRole::ActiveSplitLabel,
        surface,
        accent(),
        CONTRAST_TEXT_MIN,
        qualify_by_lightness,
    )
}

/// Accent outline marking the active split where its header is not shown.
pub fn active_split_outline(surface: Hsla) -> Hsla {
    contrast_token(
        ContrastRole::ActiveSplitOutline,
        surface,
        accent(),
        CONTRAST_NON_TEXT_MIN,
        qualify_by_lightness,
    )
}

/// Plate background - headers, active tabs
pub fn bg_plate() -> Hsla {
    apply_window_background_transparency(
        theme_color(
            hsla(0.0, 0.0, 0.04, 1.0),
            hsla(0.0, 0.0, 0.98, 1.0),
            hsla(240.0 / 360.0, 0.10, 0.03, 1.0),
            Some("bg_plate"),
        ),
        0.86,
    )
}

/// Elevated background - modals, dropdowns
pub fn bg_elevated() -> Hsla {
    apply_window_background_transparency(
        theme_color(
            hsla(0.0, 0.0, 0.08, 1.0),
            hsla(0.0, 0.0, 1.0, 1.0),
            hsla(240.0 / 360.0, 0.12, 0.08, 1.0),
            Some("bg_elevated"),
        ),
        0.9,
    )
}

/// Alternating (zebra) chat row overlay, painted over the chat background and
/// under any highlight tint. Translucent so it
/// composites on top of the themed surface in both directions: lightens dark
/// themes, darkens light ones.
pub fn bg_alternate_row() -> Hsla {
    if is_light_surface() {
        hsla(0.0, 0.0, 0.0, 0.04)
    } else {
        hsla(0.0, 0.0, 1.0, 0.03)
    }
}

/// Hover state background
pub fn bg_hover() -> Hsla {
    theme_color(
        hsla(0.0, 0.0, 0.10, 1.0),
        hsla(0.0, 0.0, 0.95, 1.0),
        hsla(240.0 / 360.0, 0.10, 0.12, 1.0),
        Some("bg_hover"),
    )
}

/// Primary text - highest contrast
pub fn text_primary() -> Hsla {
    if let Some(color) = primary_text_override() {
        return color;
    }
    inherited_text_primary()
}

/// Theme text color before the optional user override.
pub fn inherited_text_primary() -> Hsla {
    theme_color_dark_midnight(
        hsla(0.0, 0.0, 1.0, 1.0),
        hsla(0.0, 0.0, 0.08, 1.0),
        Some("text_primary"),
    )
}

/// High-contrast foreground for interactive chrome such as buttons, icons,
/// and navigation labels. This remains just below primary body text so the
/// hierarchy is visible without making controls look disabled on dark themes.
pub fn interactive_foreground() -> Hsla {
    if let Some(color) = primary_text_override() {
        return color;
    }

    match current_theme() {
        ThemeMode::Dark | ThemeMode::Midnight => hsla(0.0, 0.0, 0.92, 1.0),
        ThemeMode::Light => hsla(0.0, 0.0, 0.16, 1.0),
        ThemeMode::Custom | ThemeMode::ThemeFile => text_primary(),
    }
}

fn primary_text_override() -> Option<Hsla> {
    let key = theme_cache_key();
    PRIMARY_TEXT_OVERRIDE_CACHE.with(|cache| {
        if let Some((cached_key, color)) = cache.get()
            && cached_key == key
        {
            return color;
        }

        let color = settings::get_primary_text_color()
            .as_deref()
            .and_then(hex_to_hsla);
        cache.set(Some((key, color)));
        color
    })
}

/// Secondary text - medium emphasis
pub fn text_secondary() -> Hsla {
    theme_color_dark_midnight(
        hsla(0.0, 0.0, 0.75, 1.0),
        hsla(0.0, 0.0, 0.34, 1.0),
        Some("text_secondary"),
    )
}

/// Tertiary text - lower emphasis
pub fn text_tertiary() -> Hsla {
    match current_theme() {
        ThemeMode::Dark | ThemeMode::Midnight => hsla(0.0, 0.0, 0.64, 1.0),
        ThemeMode::Light => hsla(0.0, 0.0, 0.42, 1.0),
        ThemeMode::Custom | ThemeMode::ThemeFile => hsla(0.0, 0.0, 0.50, 1.0),
    }
}

/// Muted text - lowest emphasis (timestamps, disabled)
///
/// Light is 43% gray: 4.59:1 on `bg_hover`, 4.69:1 on `bg_void` and 4.91:1 on
/// `bg_plate`, all above WCAG AA for normal text.
pub fn text_muted() -> Hsla {
    theme_color_dark_midnight(
        hsla(0.0, 0.0, 0.52, 1.0),
        hsla(0.0, 0.0, 0.43, 1.0),
        Some("text_muted"),
    )
}

/// Text on accent color backgrounds
pub fn text_on_accent() -> Hsla {
    hsla(0.0, 0.0, 1.0, 1.0)
}

/// White or near-black, whichever reads better on a solid `background`.
pub fn text_on(background: Hsla) -> Hsla {
    let light = text_on_accent();
    let dark = hsla(0.0, 0.0, 0.08, 1.0);
    if contrast_ratio(light, background) >= contrast_ratio(dark, background) {
        light
    } else {
        dark
    }
}

/// Readable foreground for text on the current accent.
///
/// Unlike [`text_on_accent`] (always white, ~3.1:1 on the default accent),
/// this picks white or black by contrast. Pure black is intentional here: it
/// guarantees 4.5:1 normal-text contrast for every opaque accent, including
/// the narrow luminance band where white and a decorative near-black would
/// both fall just below AA.
pub fn text_on_accent_contrast() -> Hsla {
    text_on_aa(accent())
}

pub fn text_on_aa(background: Hsla) -> Hsla {
    let light = text_on_accent();
    let dark = hsla(0.0, 0.0, 0.0, 1.0);
    if contrast_ratio(light, background) >= contrast_ratio(dark, background) {
        light
    } else {
        dark
    }
}

/// Modal scrim painted behind dialogs and overlays.
///
/// One theme-aware token replacing the black 0.5/0.55/0.6/0.7 and
/// `bg_void().opacity(0.5)` backdrops: lighter on light surfaces so the dimmed
/// content stays recognizable.
pub fn backdrop() -> Hsla {
    if is_light_surface() {
        hsla(0.0, 0.0, 0.0, 0.4)
    } else {
        hsla(0.0, 0.0, 0.0, 0.6)
    }
}

/// Hover wash for interactive rows and buttons.
///
/// A translucent overlay rather than a solid fill, so it stays visible on any
/// surface. Hovering with `bg_elevated()` is invisible in Light, where it
/// equals `bg_surface()`.
pub fn hover_surface() -> Hsla {
    if is_light_surface() {
        hsla(0.0, 0.0, 0.0, 0.05)
    } else {
        hsla(0.0, 0.0, 1.0, 0.06)
    }
}

/// Pressed/active wash, one step stronger than [`hover_surface`].
pub fn pressed_surface() -> Hsla {
    if is_light_surface() {
        hsla(0.0, 0.0, 0.0, 0.10)
    } else {
        hsla(0.0, 0.0, 1.0, 0.10)
    }
}

/// White at an arbitrary alpha — general-purpose token replacing the many
/// hand-written `hsla(0.0, 0.0, 1.0, X)` literals scattered across settings
/// sections (selected-chip subtext, swatch-border hover/idle rings, etc).
/// Use `text_on_accent()` directly for the fully-opaque (alpha 1.0) case.
pub fn white_alpha(alpha: f32) -> Hsla {
    hsla(0.0, 0.0, 1.0, alpha)
}

/// Standard border
pub fn border() -> Hsla {
    theme_color(
        hsla(240.0 / 360.0, 0.04, 0.23, 1.0),
        hsla(0.0, 0.0, 0.72, 1.0),
        hsla(240.0 / 360.0, 0.10, 0.24, 1.0),
        Some("border"),
    )
}

/// Subtle border - dividers
pub fn border_subtle() -> Hsla {
    match current_theme() {
        ThemeMode::Dark => hsla(240.0 / 360.0, 0.04, 0.16, 1.0),
        ThemeMode::Light => hsla(0.0, 0.0, 0.84, 1.0),
        ThemeMode::Midnight => hsla(240.0 / 360.0, 0.08, 0.17, 1.0),
        ThemeMode::Custom | ThemeMode::ThemeFile => {
            if is_light_surface() {
                hsla(0.0, 0.0, 0.84, 1.0)
            } else {
                hsla(240.0 / 360.0, 0.04, 0.12, 1.0)
            }
        }
    }
}

/// Default accent color (fallback)
const DEFAULT_ACCENT: Hsla = Hsla {
    h: 237.0 / 360.0,
    s: 1.0,
    l: 0.75,
    a: 1.0,
};

/// Primary accent color - reads from settings
pub fn accent() -> Hsla {
    if current_theme() == ThemeMode::ThemeFile
        && let Some(hex) = settings::get_custom_theme_file_accent()
        && let Some(color) = hex_to_hsla(&hex)
    {
        return color;
    }
    let hex = settings::get_primary_accent_color();
    hex_to_hsla(&hex).unwrap_or(DEFAULT_ACCENT)
}

/// Accent for text and glyphs drawn directly on the app surfaces.
///
/// The accent is picked for dark themes and is only about 3.1:1 on white, so on
/// a light surface its lightness is walked down until it clears WCAG AA against
/// `bg_hover`, the darkest light surface text commonly sits on. Dark surfaces
/// use the accent unchanged.
pub fn accent_text() -> Hsla {
    if is_light_surface() {
        contrast_token(
            ContrastRole::AccentText,
            bg_hover(),
            accent(),
            CONTRAST_TEXT_MIN,
            qualify_by_lightness,
        )
    } else {
        accent()
    }
}

/// Accent hover state - lighter than accent
pub fn accent_hover() -> Hsla {
    lighten_hsla(accent(), 0.05)
}

/// Accent with transparency (for subtle backgrounds)
pub fn accent_transparent(alpha: f32) -> Hsla {
    let mut color = accent();
    color.a = alpha;
    color
}

/// Link color (info blue)
pub fn link() -> Hsla {
    match current_theme() {
        ThemeMode::Dark | ThemeMode::Midnight => hsla(213.0 / 360.0, 0.94, 0.68, 1.0),
        ThemeMode::Light => hsla(221.0 / 360.0, 0.83, 0.53, 1.0),
        ThemeMode::Custom | ThemeMode::ThemeFile => hsla(217.0 / 360.0, 0.72, 0.60, 1.0),
    }
}

/// A link launched from Echo during this session; kept legible in both themes.
pub fn opened_link() -> Hsla {
    match current_theme() {
        ThemeMode::Light => hsla(275.0 / 360.0, 0.65, 0.40, 1.0),
        _ => hsla(275.0 / 360.0, 0.72, 0.76, 1.0),
    }
}

/// Success color
pub fn signal_green() -> Hsla {
    match current_theme() {
        ThemeMode::Dark | ThemeMode::Midnight => hsla(0.40, 0.72, 0.52, 1.0),
        ThemeMode::Light => hsla(0.39, 0.70, 0.31, 1.0),
        ThemeMode::Custom | ThemeMode::ThemeFile => hsla(0.38, 0.65, 0.45, 1.0),
    }
}

/// Error/Live indicator color
pub fn signal_red() -> Hsla {
    match current_theme() {
        ThemeMode::Dark | ThemeMode::Midnight => hsla(0.0, 0.84, 0.62, 1.0),
        ThemeMode::Light => hsla(0.0, 0.72, 0.48, 1.0),
        ThemeMode::Custom | ThemeMode::ThemeFile => hsla(0.0, 0.75, 0.55, 1.0),
    }
}

/// Windows 11 close button hover color (#C42B1C)
pub fn signal_red_close() -> Hsla {
    hsla(0.98, 0.77, 0.44, 1.0)
}

/// Windows 11 close button active/pressed color (#861a10)
pub fn signal_red_close_active() -> Hsla {
    hsla(0.02, 0.80, 0.30, 1.0)
}

/// Warning color
pub fn signal_yellow() -> Hsla {
    match current_theme() {
        ThemeMode::Dark | ThemeMode::Midnight => hsla(0.12, 0.88, 0.60, 1.0),
        ThemeMode::Light => hsla(0.10, 0.82, 0.34, 1.0),
        ThemeMode::Custom | ThemeMode::ThemeFile => hsla(0.12, 0.75, 0.55, 1.0),
    }
}

/// Success color (alias for signal_green)
#[inline]
pub fn success() -> Hsla {
    signal_green()
}

/// Error color (alias for signal_red)
#[inline]
pub fn error() -> Hsla {
    signal_red()
}

/// Warning color (alias for signal_yellow)
#[inline]
pub fn warning() -> Hsla {
    signal_yellow()
}

/// Helper to create a highlight background from a hex color
fn hex_to_highlight(hex: &str, fallback_h: f32) -> Hsla {
    if let Some(mut color) = hex_to_hsla(hex) {
        match current_theme() {
            ThemeMode::Light => {
                color.s = color.s.min(0.4);
                color.l = 0.85;
                color.a = 0.5;
            }
            ThemeMode::Dark | ThemeMode::Midnight | ThemeMode::Custom | ThemeMode::ThemeFile => {
                color.s = color.s.min(0.5);
                color.l = 0.25;
                color.a = 0.3;
            }
        }
        color
    } else {
        hsla(fallback_h, 0.5, 0.25, 0.3)
    }
}

/// Mention highlight background - reads from settings
pub fn highlight_mention() -> Hsla {
    let hex = settings::get_mention_color();
    hex_to_highlight(&hex, 0.58)
}

/// Whisper highlight background - reads from settings
pub fn highlight_whisper() -> Hsla {
    let hex = settings::get_whisper_color();
    hex_to_highlight(&hex, 0.74)
}

/// Subscription highlight background - reads from settings
pub fn highlight_subscription() -> Hsla {
    let hex = settings::get_subscription_color();
    hex_to_highlight(&hex, 0.74)
}

/// First message highlight background - reads from settings
pub fn highlight_first_message() -> Hsla {
    let hex = settings::get_first_message_color();
    hex_to_highlight(&hex, 0.38)
}

/// Announcement highlight background
pub fn highlight_announcement() -> Hsla {
    theme_color_dark_midnight(hsla(0.12, 0.5, 0.35, 0.3), hsla(0.12, 0.4, 0.85, 0.5), None)
}

/// Raid highlight background
pub fn highlight_raid() -> Hsla {
    theme_color_dark_midnight(hsla(0.55, 0.5, 0.35, 0.3), hsla(0.55, 0.4, 0.85, 0.5), None)
}

/// Watch streak highlight background - reads from settings
pub fn highlight_watchstreak() -> Hsla {
    let hex = settings::get_watchstreak_highlight_color();
    hex_to_highlight(&hex, 0.10)
}

/// Whisper message background
pub fn whisper_bg() -> Hsla {
    let color = crate::backend::settings::get_whisper_color();
    hex_to_hsla_argb(&color, highlight_alpha::WHISPER)
        .unwrap_or_else(|| hsla(0.75, 0.6, 0.3, highlight_alpha::WHISPER))
}

/// Whisper message border
pub fn whisper_border() -> Hsla {
    let color = crate::backend::settings::get_whisper_color();
    hex_to_hsla(&color).unwrap_or_else(|| hsla(0.75, 0.6, 0.5, 1.0))
}

/// Mention highlight background - reads from settings
pub fn mention_bg() -> Hsla {
    let color = crate::backend::settings::get_mention_color();
    hex_to_hsla_argb(&color, highlight_alpha::MENTION)
        .unwrap_or_else(|| hsla(0.97, 0.6, 0.35, highlight_alpha::MENTION))
}

/// Mention highlight border - same color as background but full opacity
pub fn mention_border() -> Hsla {
    let color = crate::backend::settings::get_mention_color();
    hex_to_hsla(&color).unwrap_or_else(|| hsla(0.97, 0.6, 0.35, 1.0))
}

/// First message background
pub fn first_message_bg() -> Hsla {
    let color = crate::backend::settings::get_first_message_color();
    hex_to_hsla_argb(&color, highlight_alpha::FIRST_MESSAGE)
        .unwrap_or_else(|| hsla(0.38, 0.7, 0.45, highlight_alpha::FIRST_MESSAGE))
}

/// First message border
pub fn first_message_border() -> Hsla {
    let color = crate::backend::settings::get_first_message_color();
    hex_to_hsla(&color).unwrap_or_else(|| hsla(0.38, 0.7, 0.45, 1.0))
}

/// Reply-thread highlight background
pub fn thread_highlight_bg() -> Hsla {
    let color = crate::backend::settings::get_thread_highlight_color();
    hex_to_hsla_argb(&color, highlight_alpha::REPLY_THREAD)
        .unwrap_or_else(|| hsla(0.06, 0.5, 0.35, highlight_alpha::REPLY_THREAD))
}

/// Reply-thread highlight border
pub fn thread_highlight_border() -> Hsla {
    let color = crate::backend::settings::get_thread_highlight_color();
    hex_to_hsla(&color).unwrap_or_else(|| hsla(0.06, 0.5, 0.35, 1.0))
}

/// Highlighted message background (filter/keyword highlight, similarity, etc.)
pub fn highlighted_message_bg() -> Hsla {
    let color = crate::backend::settings::get_phrase_highlight_color();
    hex_to_hsla_argb(&color, highlight_alpha::HIGHLIGHTED)
        .unwrap_or_else(|| hsla(0.12, 0.9, 0.55, highlight_alpha::HIGHLIGHTED))
}

/// Channel-point redemption background (teal)
pub fn redeem_bg() -> Hsla {
    let color = crate::backend::settings::get_redeem_highlight_color();
    hex_to_hsla_argb(&color, highlight_alpha::REDEEMED)
        .unwrap_or_else(|| hsla(0.52, 0.67, 0.33, highlight_alpha::REDEEMED))
}

/// Self-message background (blue)
pub fn self_message_bg() -> Hsla {
    let color = crate::backend::settings::get_self_message_highlight_color();
    hex_to_hsla_argb(&color, highlight_alpha::SELF_MESSAGE)
        .unwrap_or_else(|| hsla(0.58, 1.0, 0.43, highlight_alpha::SELF_MESSAGE))
}

/// Self-message border (blue, opaque)
pub fn self_message_border() -> Hsla {
    let color = crate::backend::settings::get_self_message_highlight_color();
    hex_to_hsla(&color).unwrap_or_else(|| hsla(0.58, 1.0, 0.43, 1.0))
}

/// Elevated / Hype Chat background (amber)
pub fn elevated_bg() -> Hsla {
    let color = crate::backend::settings::get_elevated_highlight_color();
    hex_to_hsla_argb(&color, highlight_alpha::ELEVATED)
        .unwrap_or_else(|| hsla(0.1, 1.0, 0.63, highlight_alpha::ELEVATED))
}

/// Elevated / Hype Chat border (amber, opaque)
pub fn elevated_border() -> Hsla {
    let color = crate::backend::settings::get_elevated_highlight_color();
    hex_to_hsla(&color).unwrap_or_else(|| hsla(0.1, 1.0, 0.63, 1.0))
}

/// Subscription message background
pub fn subscription_bg() -> Hsla {
    let color = crate::backend::settings::get_subscription_color();
    hex_to_hsla_argb(&color, highlight_alpha::SUBSCRIPTION)
        .unwrap_or_else(|| hsla(0.75, 0.6, 0.5, highlight_alpha::SUBSCRIPTION))
}

/// Subscription message border
pub fn subscription_border() -> Hsla {
    let color = crate::backend::settings::get_subscription_color();
    hex_to_hsla(&color).unwrap_or_else(|| hsla(0.75, 0.6, 0.5, 1.0))
}

/// Raid message background
pub fn raid_bg() -> Hsla {
    let color = crate::backend::settings::get_raid_highlight_color();
    hex_to_hsla_argb(&color, highlight_alpha::HIGHLIGHTED)
        .unwrap_or_else(|| hsla(0.75, 0.6, 0.5, highlight_alpha::HIGHLIGHTED))
}

/// Bits badge background
pub fn bits_bg() -> Hsla {
    let color = crate::backend::settings::get_bits_highlight_color();
    hex_to_hsla_argb(&color, highlight_alpha::HIGHLIGHTED)
        .unwrap_or_else(|| hsla(0.12, 0.9, 0.55, highlight_alpha::HIGHLIGHTED))
}

/// Announcement background (default; see [`announcement_bg_for`] for Helix color variants)
pub fn announcement_bg() -> Hsla {
    let color = crate::backend::settings::get_announcement_highlight_color();
    hex_to_hsla_argb(&color, highlight_alpha::ANNOUNCEMENT)
        .unwrap_or_else(|| hsla(0.85, 1.0, 0.7, highlight_alpha::ANNOUNCEMENT))
}

/// Announcement background for a specific Helix announcement color variant.
///
/// Maps the `msg-param-color` tag (PRIMARY/BLUE/GREEN/ORANGE/PURPLE) to
/// distinct highlight colors; PRIMARY falls back to the configured
/// announcement color.
pub fn announcement_bg_for(variant: Option<&str>) -> Hsla {
    let hex = match variant.map(|v| v.trim().to_ascii_uppercase()).as_deref() {
        Some("BLUE") => "#6694FF",
        Some("GREEN") => "#60FF60",
        Some("ORANGE") => "#E9D200",
        Some("PURPLE") => "#FF66ED",
        _ => return announcement_bg(),
    };
    hex_to_hsla_argb(hex, highlight_alpha::ANNOUNCEMENT).unwrap_or_else(announcement_bg)
}

/// Watch streak background
pub fn watchstreak_bg() -> Hsla {
    let color = crate::backend::settings::get_watchstreak_highlight_color();
    hex_to_hsla_argb(&color, highlight_alpha::WATCH_STREAK)
        .unwrap_or_else(|| hsla(0.10, 0.9, 0.55, highlight_alpha::WATCH_STREAK))
}

/// Watch streak border
pub fn watchstreak_border() -> Hsla {
    let color = crate::backend::settings::get_watchstreak_highlight_color();
    hex_to_hsla(&color).unwrap_or_else(|| hsla(0.10, 0.9, 0.55, 1.0))
}

/// Phrase highlight background (muted red)
pub fn phrase_bg() -> Hsla {
    let color = crate::backend::settings::get_phrase_highlight_color();
    hex_to_hsla_argb(&color, highlight_alpha::PHRASE)
        .unwrap_or_else(|| hsla(0.98, 0.34, 0.37, highlight_alpha::PHRASE))
}

pub fn custom_highlight_bg(color: Option<&str>) -> Option<Hsla> {
    color
        .and_then(|color| hex_to_hsla_argb(color, highlight_alpha::PHRASE))
        .or_else(|| Some(phrase_bg()))
}

/// AutoMod-held message background. Honors any explicit alpha in
/// `automod_highlight_color`; the default `#404040` grey renders as a
/// semi-transparent wash so caught text stays legible.
pub fn automod_bg() -> Hsla {
    let color = crate::backend::settings::get_automod_highlight_color();
    hex_to_hsla_argb(&color, highlight_alpha::AUTOMOD)
        .unwrap_or_else(|| hsla(0.0, 0.0, 0.25, highlight_alpha::AUTOMOD))
}

/// AutoMod-held message left border: the configured grey at a stronger,
/// near-opaque alpha so the bar reads distinctly from the row wash.
pub fn automod_border() -> Hsla {
    let color = crate::backend::settings::get_automod_highlight_color();
    let mut border = hex_to_hsla(&color).unwrap_or_else(|| hsla(0.0, 0.0, 0.35, 1.0));
    border.a = 0.85;
    border
}

/// Suspicious-user ("low trust") row background. Restricted users get a red
/// wash; monitored users a softer amber — both distinct from the AutoMod grey.
/// Translucent so a single value composites acceptably in light and dark.
pub fn low_trust_bg(restricted: bool) -> Hsla {
    if restricted {
        hsla(0.0, 0.65, 0.45, 0.20)
    } else {
        hsla(0.09, 0.75, 0.5, 0.16)
    }
}

/// Suspicious-user ("low trust") left border, matching [`low_trust_bg`].
pub fn low_trust_border(restricted: bool) -> Hsla {
    if restricted {
        hsla(0.0, 0.7, 0.5, 0.9)
    } else {
        hsla(0.09, 0.8, 0.55, 0.85)
    }
}

pub fn custom_highlight_border(color: Option<&str>) -> Option<Hsla> {
    color.and_then(hex_to_hsla).or_else(|| Some(warning()))
}

/// WCAG 2.x minimum contrast ratio for focus indicators, the selection rail,
/// chevrons, and other boundaries that carry meaning on their own.
pub const CONTRAST_NON_TEXT_MIN: f32 = 3.0;

/// WCAG AA minimum for normal-size text.
pub const CONTRAST_TEXT_MIN: f32 = 4.5;

/// Minimum contrast for a purely structural 1px rule.
///
/// Rules separate content that is already labelled, so they are deliberately
/// held below [`CONTRAST_NON_TEXT_MIN`]: a subsection divider forced to 3:1
/// reads as a bright bar rather than a hairline. This floor only guarantees the
/// line never disappears into an adversarial custom palette.
pub const CONTRAST_RULE_MIN: f32 = 1.45;

/// Minimum contrast for the subtle rules that fence a settings group body.
pub const CONTRAST_SUBTLE_RULE_MIN: f32 = 1.2;

/// WCAG relative luminance of a color, ignoring its alpha channel.
pub fn relative_luminance(color: Hsla) -> f32 {
    #[inline]
    fn linearize(channel: f32) -> f32 {
        if channel <= 0.03928 {
            channel / 12.92
        } else {
            ((channel + 0.055) / 1.055).powf(2.4)
        }
    }

    let rgba = Rgba::from(color);
    0.2126 * linearize(rgba.r) + 0.7152 * linearize(rgba.g) + 0.0722 * linearize(rgba.b)
}

/// WCAG contrast ratio (1.0 ..= 21.0) between two colors, ignoring alpha.
pub fn contrast_ratio(a: Hsla, b: Hsla) -> f32 {
    let (first, second) = (relative_luminance(a), relative_luminance(b));
    let (lighter, darker) = if first >= second {
        (first, second)
    } else {
        (second, first)
    };
    (lighter + 0.05) / (darker + 0.05)
}

/// Which settings token a memo slot holds. Doubles as the slot index, so each
/// role keeps its own most-recent (surface, preferred) resolution.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ContrastRole {
    FocusRing = 0,
    SelectionRail = 1,
    Rule = 2,
    SubtleRule = 3,
    ActiveSplitLabel = 4,
    ActiveSplitOutline = 5,
    AccentText = 6,
    GroupBorder = 7,
    SelectionForeground = 8,
    SwitchOnTrack = 9,
    SwitchDisabledOnTrack = 10,
    SwitchOffTrack = 11,
    SwitchDisabledTrack = 12,
    SwitchAccentOnTrack = 13,
    SwitchAccentDisabledOnTrack = 14,
}

const CONTRAST_ROLE_COUNT: usize = 15;

#[derive(Clone, Copy)]
struct ContrastToken {
    key: ThemeCacheKey,
    surface: Hsla,
    preferred: Hsla,
    resolved: Hsla,
}

thread_local! {
    /// Direct-mapped memo, one slot per [`ContrastRole`].
    ///
    /// The theme cache key is an atomic pair, so a hit costs two relaxed loads
    /// and never takes the settings mutex. The accent itself is deliberately
    /// *not* memoized on that key: `SettingsEvent::AccentChanged` does not bump
    /// the theme version, and the Appearance page previews accent edits live.
    static CONTRAST_TOKENS: Cell<[Option<ContrastToken>; CONTRAST_ROLE_COUNT]> =
        const { Cell::new([None; CONTRAST_ROLE_COUNT]) };
}

fn contrast_token(
    role: ContrastRole,
    surface: Hsla,
    preferred: Hsla,
    minimum: f32,
    resolve: fn(Hsla, Hsla, f32) -> Hsla,
) -> Hsla {
    let key = theme_cache_key();
    let slot = role as usize;

    if let Some(entry) = CONTRAST_TOKENS.with(|cache| cache.get()[slot])
        && entry.key == key
        && entry.surface == surface
        && entry.preferred == preferred
    {
        return entry.resolved;
    }

    let resolved = resolve(preferred, surface, minimum);
    CONTRAST_TOKENS.with(|cache| {
        let mut slots = cache.get();
        slots[slot] = Some(ContrastToken {
            key,
            surface,
            preferred,
            resolved,
        });
        cache.set(slots);
    });
    resolved
}

/// Substitute a readable foreground when `preferred` (usually a custom accent)
/// cannot clear `minimum` against `surface`.
fn qualify_by_substitution(preferred: Hsla, surface: Hsla, minimum: f32) -> Hsla {
    for candidate in [preferred, interactive_foreground(), text_primary()] {
        if contrast_ratio(candidate, surface) >= minimum {
            return candidate;
        }
    }
    if relative_luminance(surface) > 0.18 {
        hsla(0.0, 0.0, 0.0, 1.0)
    } else {
        hsla(0.0, 0.0, 1.0, 1.0)
    }
}

/// Keep a rule's own hue/saturation and walk its lightness away from the
/// surface until it clears `minimum`.
///
/// Substituting a text color here would turn every divider into a bar; nudging
/// lightness keeps the line reading as chrome.
fn qualify_by_lightness(preferred: Hsla, surface: Hsla, minimum: f32) -> Hsla {
    let mut rule = preferred;
    let away_from_surface = if surface.l > 0.5 { -0.04 } else { 0.04 };
    for _ in 0..16 {
        if contrast_ratio(rule, surface) >= minimum {
            return rule;
        }
        let next = (rule.l + away_from_surface).clamp(0.0, 1.0);
        if (next - rule.l).abs() < f32::EPSILON {
            break;
        }
        rule.l = next;
    }
    rule
}

fn qualify_switch_track(preferred: Hsla, surface: Hsla, minimum: f32) -> Hsla {
    let candidate = qualify_by_lightness(preferred, surface, minimum);
    if contrast_ratio(candidate, surface) >= minimum {
        candidate
    } else {
        text_on_aa(surface)
    }
}

/// Model nearest-byte framebuffer conversion and truncating color exports.
fn quantized_switch_color(color: Hsla, round: bool) -> Hsla {
    let color = Rgba::from(color);
    let channel = |value: f32| {
        let scaled = value.clamp(0.0, 1.0) * 255.0;
        (if round {
            scaled.round()
        } else {
            scaled.floor()
        }) / 255.0
    };
    Hsla::from(Rgba {
        r: channel(color.r),
        g: channel(color.g),
        b: channel(color.b),
        a: 1.0,
    })
}

fn switch_track_meets_contrast(track: Hsla, surface: Hsla, minimum: f32) -> bool {
    contrast_ratio(track, surface) >= minimum
        && [true, false].into_iter().all(|round| {
            contrast_ratio(
                quantized_switch_color(track, round),
                quantized_switch_color(surface, round),
            ) >= minimum
        })
}

/// At fixed hue/saturation, luminance is monotonic in HSL lightness. Retain
/// each boundary's valid side in float and 8-bit space, then choose the
/// smallest lightness change. No extra hue/saturation change or margin.
fn minimal_switch_lightness(preferred: Hsla, surface: Hsla, minimum: f32) -> Option<Hsla> {
    if switch_track_meets_contrast(preferred, surface, minimum) {
        return Some(preferred);
    }
    let mut closest: Option<Hsla> = None;
    for endpoint in [0.0, 1.0] {
        let mut valid = Hsla {
            l: endpoint,
            ..preferred
        };
        if !switch_track_meets_contrast(valid, surface, minimum) {
            continue;
        }
        let mut invalid = preferred.l;
        for _ in 0..24 {
            let candidate = Hsla {
                l: (valid.l + invalid) * 0.5,
                ..preferred
            };
            if switch_track_meets_contrast(candidate, surface, minimum) {
                valid = candidate;
            } else {
                invalid = candidate.l;
            }
        }
        if closest.is_none_or(|old| (valid.l - preferred.l).abs() < (old.l - preferred.l).abs()) {
            closest = Some(valid);
        }
    }
    closest
}

fn qualify_accent_switch_track(preferred: Hsla, surface: Hsla, minimum: f32) -> Hsla {
    let opaque = opaque_composite(surface, preferred);
    minimal_switch_lightness(opaque, surface, minimum).unwrap_or_else(|| {
        let neutral = settings_switch_on_track(surface.l > 0.5, true);
        minimal_switch_lightness(neutral, surface, minimum)
            .unwrap_or_else(|| qualify_switch_track(neutral, surface, minimum))
    })
}

fn switch_colors_distinct(first: Hsla, second: Hsla) -> bool {
    let first_rgb = Rgba::from(first);
    let second_rgb = Rgba::from(second);
    contrast_ratio(first, second) >= 1.2
        || (first_rgb.r - second_rgb.r)
            .abs()
            .max((first_rgb.g - second_rgb.g).abs())
            .max((first_rgb.b - second_rgb.b).abs())
            >= 0.08
}

fn qualify_disabled_accent_switch_track(preferred: Hsla, surface: Hsla, minimum: f32) -> Hsla {
    let enabled = qualify_accent_switch_track(preferred, surface, minimum);
    let light = surface.l > 0.5;
    let neutral = settings_switch_on_track(light, false);
    let off = qualify_switch_track(settings_switch_off_track(light, false), surface, minimum);
    let mut muted = opaque_composite(surface, preferred);
    muted.s *= 0.25;
    // Most accents settle here. Alternate neutral lightness targets handle
    // gray/black/white accents or custom surfaces that collide with that mix.
    let lightness = [
        muted.l * 0.35 + neutral.l * 0.65,
        neutral.l,
        if light { 0.45 } else { 0.55 },
        if light { 0.18 } else { 0.88 },
        0.0,
        1.0,
    ];
    for l in lightness {
        if let Some(candidate) = minimal_switch_lightness(Hsla { l, ..muted }, surface, minimum)
            && switch_colors_distinct(candidate, enabled)
            && switch_colors_distinct(candidate, off)
        {
            return candidate;
        }
    }
    minimal_switch_lightness(neutral, surface, minimum)
        .unwrap_or_else(|| qualify_switch_track(neutral, surface, minimum))
}

/// Keyboard focus-ring color for a settings control painted on `surface`.
///
/// Prefers the user's accent and falls back to `interactive_foreground` and
/// then `text_primary` when a low-contrast custom accent would make the ring
/// invisible. Selection must never be signalled by color alone, so callers pair
/// this with the rail, weight, and shape treatments.
pub fn settings_focus_ring(surface: Hsla) -> Hsla {
    contrast_token(
        ContrastRole::FocusRing,
        surface,
        accent(),
        CONTRAST_NON_TEXT_MIN,
        qualify_by_substitution,
    )
}

/// Solid selection-rail color for the current settings navigation location.
pub fn settings_selection_rail(surface: Hsla) -> Hsla {
    contrast_token(
        ContrastRole::SelectionRail,
        surface,
        accent(),
        CONTRAST_NON_TEXT_MIN,
        qualify_by_substitution,
    )
}

/// Opaque accent wash for the selected Settings navigation item.
///
/// Materializing the blend keeps the selected row's contrast independent of
/// its parent opacity and lets the label use an ordinary readable foreground
/// instead of white text on an arbitrary user-selected accent.
pub fn settings_selection_fill(surface: Hsla) -> Hsla {
    opaque_composite(surface, accent_transparent(0.10))
}

/// Label and icon foreground for [`settings_selection_fill`].
pub fn settings_selection_foreground(surface: Hsla) -> Hsla {
    let fill = settings_selection_fill(surface);
    contrast_token(
        ContrastRole::SelectionForeground,
        fill,
        text_primary(),
        CONTRAST_TEXT_MIN,
        qualify_by_substitution,
    )
}

/// Structural 1px rule under a settings subsection heading.
pub fn settings_rule(surface: Hsla) -> Hsla {
    contrast_token(
        ContrastRole::Rule,
        surface,
        border(),
        CONTRAST_RULE_MIN,
        qualify_by_lightness,
    )
}

/// Subtle top/bottom rules fencing a settings group body.
pub fn settings_group_rule(surface: Hsla) -> Hsla {
    contrast_token(
        ContrastRole::SubtleRule,
        surface,
        border_subtle(),
        CONTRAST_SUBTLE_RULE_MIN,
        qualify_by_lightness,
    )
}

/// Resolve a source-over blend to an opaque color.
///
/// Settings selection and group fills need their actual painted color for
/// contrast decisions; retaining an alpha channel would make the result depend
/// on a parent surface outside the token's contract.
fn opaque_composite(base: Hsla, overlay: Hsla) -> Hsla {
    let mut base = Rgba::from(base);
    base.a = 1.0;
    let mut overlay = Rgba::from(overlay);
    overlay.a = overlay.a.clamp(0.0, 1.0);
    let mut result = base.blend(overlay);
    result.a = 1.0;
    Hsla::from(result)
}

/// Twitch purple (for whispers, subs, raids)
pub fn twitch_purple() -> Hsla {
    hsla(0.75, 0.6, 0.5, 1.0)
}

/// Info blue (for returning chatter)
pub fn info() -> Hsla {
    hsla(0.55, 0.7, 0.55, 1.0)
}

#[cfg(test)]
mod settings_contrast_tests {
    use super::*;
    use core::prelude::v1::test;

    fn rgb(r: u8, g: u8, b: u8) -> Hsla {
        Hsla::from(Rgba {
            r: r as f32 / 255.0,
            g: g as f32 / 255.0,
            b: b as f32 / 255.0,
            a: 1.0,
        })
    }

    #[test]
    fn settings_switch_palette_clears_non_text_contrast_on_light_and_dark_groups() {
        // #F8F9FB is the resolved default light settings group fill.
        let light_group = rgb(248, 249, 251);
        let dark_group = rgb(15, 15, 15);

        for (track, group) in [
            (settings_switch_on_track(true, true), light_group),
            (settings_switch_on_track(true, false), light_group),
            (settings_switch_on_track(false, true), dark_group),
            (settings_switch_on_track(false, false), dark_group),
            (settings_switch_off_track(true, true), light_group),
            (settings_switch_off_track(true, false), light_group),
            (settings_switch_off_track(false, true), dark_group),
            (settings_switch_off_track(false, false), dark_group),
        ] {
            assert!(contrast_ratio(track, group) >= CONTRAST_NON_TEXT_MIN);
            assert!(contrast_ratio(text_on(track), track) >= CONTRAST_NON_TEXT_MIN);
        }

        // The on state must remain neutral even when the accent changes.
        assert_eq!(settings_switch_on_track(true, true).s, 0.0);
        assert_eq!(settings_switch_on_track(false, true).s, 0.0);
        assert_ne!(
            settings_switch_on_track(true, true),
            settings_switch_on_track(true, false)
        );
        assert_ne!(
            settings_switch_on_track(false, true),
            settings_switch_on_track(false, false)
        );
        assert_ne!(
            settings_switch_off_track(false, true),
            settings_switch_off_track(false, false)
        );
    }

    #[test]
    fn opaque_selection_fill_keeps_a_normal_text_foreground_readable() {
        let surface = rgb(255, 255, 255);
        let accent = rgb(127, 134, 255).opacity(0.10);
        let fill = opaque_composite(surface, accent);
        assert_eq!(fill.a, 1.0);
        assert!(contrast_ratio(text_on(fill), fill) >= CONTRAST_TEXT_MIN);
    }

    #[test]
    fn primary_accent_foreground_clears_aa_for_light_dark_and_mid_luminance_accents() {
        for accent in [
            rgb(127, 134, 255),
            rgb(24, 54, 128),
            rgb(230, 195, 60),
            rgb(117, 117, 117),
        ] {
            assert!(contrast_ratio(text_on_aa(accent), accent) >= CONTRAST_TEXT_MIN);
        }
    }

    #[test]
    fn all_switch_states_remain_visible_on_custom_surfaces() {
        for surface in [
            rgb(107, 114, 128),
            rgb(123, 132, 148),
            rgb(127, 134, 255),
            rgb(0, 0, 220),
            rgb(245, 245, 245),
        ] {
            for preferred in [
                settings_switch_off_track(true, true),
                settings_switch_off_track(true, false),
                settings_switch_off_track(false, true),
                settings_switch_on_track(true, true),
                settings_switch_on_track(true, false),
                settings_switch_on_track(false, true),
                settings_switch_on_track(false, false),
            ] {
                let track = qualify_switch_track(preferred, surface, CONTRAST_NON_TEXT_MIN);
                assert!(contrast_ratio(track, surface) >= CONTRAST_NON_TEXT_MIN);
                assert!(contrast_ratio(text_on(track), track) >= CONTRAST_NON_TEXT_MIN);
            }
        }
    }

    fn switch_accents() -> [Hsla; 9] {
        [
            rgb(127, 134, 255),
            rgb(255, 212, 0),
            rgb(30, 58, 138),
            rgb(255, 20, 147),
            rgb(0, 255, 0),
            rgb(128, 128, 128),
            rgb(255, 255, 255),
            rgb(0, 0, 0),
            rgb(127, 134, 255).alpha(128.0 / 255.0),
        ]
    }

    #[test]
    fn accent_switch_matrix_preserves_contrast_hue_and_disabled_distinction() {
        for surface in [
            rgb(248, 249, 251),
            rgb(15, 15, 15),
            rgb(11, 11, 14),
            rgb(107, 114, 128),
            rgb(200, 210, 190),
        ] {
            for accent in switch_accents() {
                let enabled = qualify_accent_switch_track(accent, surface, CONTRAST_NON_TEXT_MIN);
                let disabled =
                    qualify_disabled_accent_switch_track(accent, surface, CONTRAST_NON_TEXT_MIN);
                let off = qualify_switch_track(
                    settings_switch_off_track(surface.l > 0.5, false),
                    surface,
                    CONTRAST_NON_TEXT_MIN,
                );
                for track in [enabled, disabled] {
                    assert_eq!(track.a, 1.0);
                    assert!(contrast_ratio(track, surface) >= CONTRAST_NON_TEXT_MIN);
                    assert!(contrast_ratio(text_on(track), track) >= CONTRAST_NON_TEXT_MIN);
                }
                let composite = opaque_composite(surface, accent);
                assert_eq!(enabled.h, composite.h);
                assert_eq!(enabled.s, composite.s);
                assert!(switch_colors_distinct(enabled, disabled));
                assert!(
                    switch_colors_distinct(disabled, off),
                    "surface={surface:?}, accent={accent:?}, disabled={disabled:?}, off={off:?}"
                );
            }
        }
    }

    #[test]
    fn accent_switch_changes_only_the_minimum_necessary_lightness() {
        for surface in [rgb(248, 249, 251), rgb(15, 15, 15), rgb(128, 128, 128)] {
            for accent in switch_accents() {
                let preferred = opaque_composite(surface, accent);
                let track =
                    minimal_switch_lightness(preferred, surface, CONTRAST_NON_TEXT_MIN).unwrap();
                if switch_track_meets_contrast(preferred, surface, CONTRAST_NON_TEXT_MIN) {
                    assert_eq!(track, preferred);
                } else {
                    let closer = Hsla {
                        l: track.l + (preferred.l - track.l).signum() * 0.00001,
                        ..track
                    };
                    assert!(!switch_track_meets_contrast(
                        closer,
                        surface,
                        CONTRAST_NON_TEXT_MIN
                    ));
                    for step in 0..=1000 {
                        let candidate = Hsla {
                            l: step as f32 / 1000.0,
                            ..preferred
                        };
                        if switch_track_meets_contrast(candidate, surface, CONTRAST_NON_TEXT_MIN) {
                            assert!(
                                (track.l - preferred.l).abs()
                                    <= (candidate.l - preferred.l).abs() + 0.000001
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn neutral_and_off_switch_tokens_remain_identical_for_every_accent() {
        use settings::SwitchOnStyle;
        for surface in [rgb(248, 249, 251), rgb(15, 15, 15), rgb(107, 114, 128)] {
            for accent in switch_accents() {
                for enabled in [true, false] {
                    for on in [true, false] {
                        let old = qualify_switch_track(
                            if on {
                                settings_switch_on_track(surface.l > 0.5, enabled)
                            } else {
                                settings_switch_off_track(surface.l > 0.5, enabled)
                            },
                            surface,
                            CONTRAST_NON_TEXT_MIN,
                        );
                        let neutral = resolve_switch_track(
                            SwitchOnStyle::Neutral,
                            on,
                            enabled,
                            surface,
                            accent,
                        );
                        assert_eq!(neutral, old);
                        assert_eq!(text_on(neutral), text_on(old));
                        if !on {
                            assert_eq!(
                                resolve_switch_track(
                                    SwitchOnStyle::Accent,
                                    false,
                                    enabled,
                                    surface,
                                    accent
                                ),
                                old
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn switch_argb_sample_keeps_alpha_until_it_is_composited() {
        let translucent = hex_to_hsla_argb("#807F86FF", 1.0).unwrap();
        let opaque = hex_to_hsla_argb("#7F86FF", 1.0).unwrap();
        assert_eq!(translucent.a, 128.0 / 255.0);
        for surface in [rgb(248, 249, 251), rgb(15, 15, 15)] {
            let track = qualify_accent_switch_track(translucent, surface, CONTRAST_NON_TEXT_MIN);
            assert_eq!(track.a, 1.0);
            assert_ne!(
                track,
                qualify_accent_switch_track(opaque, surface, CONTRAST_NON_TEXT_MIN)
            );
        }
    }

    #[test]
    fn random_accent_sweep_clears_contrast_after_eight_bit_conversion() {
        let mut seed = 0x82e7_405du32;
        for surface in [rgb(248, 249, 251), rgb(15, 15, 15)] {
            for _ in 0..4096 {
                seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                let accent = rgb((seed >> 24) as u8, (seed >> 16) as u8, (seed >> 8) as u8)
                    .alpha((seed & 255) as f32 / 255.0);
                for track in [
                    qualify_accent_switch_track(accent, surface, CONTRAST_NON_TEXT_MIN),
                    qualify_disabled_accent_switch_track(accent, surface, CONTRAST_NON_TEXT_MIN),
                ] {
                    for round in [true, false] {
                        let rounded_track = quantized_switch_color(track, round);
                        let rounded_surface = quantized_switch_color(surface, round);
                        let rounded_knob = quantized_switch_color(text_on(track), round);
                        assert!(
                            contrast_ratio(rounded_track, rounded_surface) >= CONTRAST_NON_TEXT_MIN
                        );
                        assert!(
                            contrast_ratio(rounded_knob, rounded_track) >= CONTRAST_NON_TEXT_MIN
                        );
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod user_color_tests {
    use super::*;
    use core::prelude::v1::test;

    /// The built-in Light, Dark and Midnight chat backgrounds (`bg_void`).
    fn chat_backgrounds() -> [Hsla; 3] {
        [
            hsla(0.0, 0.0, 0.96, 1.0),
            hsla(0.0, 0.0, 0.02, 1.0),
            hsla(240.0 / 360.0, 0.15, 0.01, 1.0),
        ]
    }

    fn rgb_hex(hex: u32) -> Hsla {
        Hsla::from(Rgba {
            r: ((hex >> 16) & 0xFF) as f32 / 255.0,
            g: ((hex >> 8) & 0xFF) as f32 / 255.0,
            b: (hex & 0xFF) as f32 / 255.0,
            a: 1.0,
        })
    }

    fn to_hex(color: Hsla) -> u32 {
        let color = Rgba::from(color);
        let byte = |value: f32| (value.clamp(0.0, 1.0) * 255.0).round() as u32;
        (byte(color.r) << 16) | (byte(color.g) << 8) | byte(color.b)
    }

    /// (input, on the Light chat background, on the Dark chat background):
    /// Twitch's standard username palette, then white, black, yellow, cyan,
    /// navy, gray, pink, midnight blue and purple.
    const PINNED: [(u32, u32, u32); 24] = [
        (0xFF0000, 0xE30000, 0xFF0000),
        (0x0000FF, 0x0000FF, 0x6161FF),
        (0x00FF00, 0x008300, 0x00FF00),
        (0xB22222, 0xB22222, 0xD93A3A),
        (0xFF7F50, 0xD13800, 0xFF7F50),
        (0x9ACD32, 0x5C7A1E, 0x9ACD32),
        (0xFF4500, 0xD13800, 0xFF4500),
        (0x2E8B57, 0x2A7F50, 0x2E8B57),
        (0xDAA520, 0x8E6B15, 0xDAA520),
        (0xD2691E, 0xB05819, 0xD2691E),
        (0x5F9EA0, 0x48797A, 0x5F9EA0),
        (0x1E90FF, 0x006FDA, 0x1E90FF),
        (0xFF69B4, 0xDC006E, 0xFF69B4),
        (0x8A2BE2, 0x8A2BE2, 0x9D4DE7),
        (0x00FF7F, 0x008241, 0x00FF7F),
        (0xFFFFFF, 0x717171, 0xFFFFFF),
        (0x000000, 0x000000, 0x767676),
        (0xFFFF00, 0x757500, 0xFFFF00),
        (0x00FFFF, 0x007E7E, 0x00FFFF),
        (0x000080, 0x000080, 0x6161FF),
        (0x808080, 0x717171, 0x808080),
        (0xFFC0CB, 0xE20028, 0xFFC0CB),
        (0x191970, 0x191970, 0x6868DD),
        (0x800080, 0x800080, 0xD100D1),
    ];

    #[test]
    fn user_colors_on_default_chat_backgrounds_match_pinned_values() {
        let [light, dark, _] = chat_backgrounds();
        for (input, on_light, on_dark) in PINNED {
            let color = rgb_hex(input);
            assert_eq!(
                to_hex(readable_user_color(color, light)),
                on_light,
                "#{input:06X} on light"
            );
            assert_eq!(
                to_hex(readable_user_color(color, dark)),
                on_dark,
                "#{input:06X} on dark"
            );
        }
    }

    fn assert_readable(color: Hsla, surface: Hsla) {
        let out = readable_user_color(color, surface);
        let context = format!("color={color:?}, surface={surface:?}, out={out:?}");
        assert!(
            contrast_ratio(out, surface) >= CONTRAST_TEXT_MIN,
            "{context}"
        );
        assert_eq!(
            (out.h, out.s, out.a),
            (color.h, color.s, color.a),
            "{context}"
        );
        if contrast_ratio(color, surface) >= CONTRAST_TEXT_MIN {
            assert_eq!(out, color, "{context}");
            return;
        }
        if text_on_aa(surface).l == 0.0 {
            assert!(out.l < color.l, "{context}");
        } else {
            assert!(out.l > color.l, "{context}");
        }
        let step = (color.l - out.l).abs().min(0.001);
        let closer = Hsla {
            l: out.l + (color.l - out.l).signum() * step,
            ..out
        };
        assert!(
            contrast_ratio(closer, surface) < CONTRAST_TEXT_MIN,
            "{context}"
        );
    }

    #[test]
    fn normalized_user_colors_are_readable_and_keep_hue_saturation_and_alpha() {
        let mut surfaces = chat_backgrounds().to_vec();
        // Custom palettes: mid gray (where black and white both barely clear
        // 4.5:1), saturated yellow and blue (HSL lightness 0.5 either way),
        // and tinted dark and light surfaces.
        surfaces.extend([0x767676, 0xFFFF00, 0x0000FF, 0x1E1E2E, 0xFDF6E3].map(rgb_hex));

        let mut colors: Vec<Hsla> = PINNED.iter().map(|&(input, _, _)| rgb_hex(input)).collect();
        colors.extend(colors.clone().into_iter().map(|color| color.alpha(0.5)));
        let mut seed = 0x5eed_c010u32;
        for _ in 0..4096 {
            seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
            colors.push(rgb_hex(seed >> 8).alpha((seed & 255) as f32 / 255.0));
        }

        for surface in surfaces {
            for &color in &colors {
                assert_readable(color, surface);
            }
        }
    }
}

/// Create an HSLA color from components
/// h: 0.0-1.0 (hue)
/// s: 0.0-1.0 (saturation)
/// l: 0.0-1.0 (lightness)
/// a: 0.0-1.0 (alpha)
fn hsla(h: f32, s: f32, l: f32, a: f32) -> Hsla {
    Hsla { h, s, l, a }
}
