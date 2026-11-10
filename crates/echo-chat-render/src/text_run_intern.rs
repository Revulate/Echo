//! Shared GPUI text-run style construction.
//!
//! `gpui::TextRun` includes a byte length, so complete runs cannot be reused
//! wholesale across arbitrary message segments. The expensive immutable parts —
//! the common font-family strings and style-to-run construction — are centralized
//! here so layout code does not allocate a new font-family string for every run.

use gpui::{Font, FontWeight, Hsla, SharedString, StrikethroughStyle, TextRun, UnderlineStyle, px};
use std::sync::OnceLock;

use super::font_metrics::DEFAULT_FONT_FAMILY;
use super::layout::TextRunData;

static SYSTEM_FONT_FAMILY: OnceLock<SharedString> = OnceLock::new();
static UI_FONT_FAMILY: OnceLock<SharedString> = OnceLock::new();
static MONO_FONT_FAMILY: OnceLock<SharedString> = OnceLock::new();

/// Replace the default families before any text is laid out; returns `false`
/// when some text already used a default. The defaults are Windows families
/// (GPUI's `.SystemUIFont` is Segoe UI there), which a Linux host replaces
/// with families it found installed.
pub fn install_font_families(ui: SharedString, mono: SharedString) -> bool {
    let chat_installed = SYSTEM_FONT_FAMILY.set(ui.clone()).is_ok();
    let ui_installed = UI_FONT_FAMILY.set(ui).is_ok();
    let mono_installed = MONO_FONT_FAMILY.set(mono).is_ok();
    chat_installed && ui_installed && mono_installed
}

/// Chat message text.
#[inline]
pub fn system_font_family() -> SharedString {
    SYSTEM_FONT_FAMILY
        .get_or_init(|| DEFAULT_FONT_FAMILY.into())
        .clone()
}

/// Application chrome and the composer.
#[inline]
pub fn ui_font_family() -> SharedString {
    UI_FONT_FAMILY.get_or_init(|| "Segoe UI".into()).clone()
}

/// ASCII art and other monospace chat text.
#[inline]
pub fn mono_font_family() -> SharedString {
    MONO_FONT_FAMILY
        .get_or_init(|| "Cascadia Code".into())
        .clone()
}

#[inline]
pub fn gpui_run_from_data(run: &TextRunData) -> TextRun {
    TextRun {
        len: run.len,
        font: Font {
            family: run.font_family.clone(),
            weight: run.font_weight,
            ..Default::default()
        },
        color: run.color,
        background_color: run.background_color,
        underline: if run.underline {
            Some(UnderlineStyle {
                thickness: px(1.0),
                color: Some(run.color),
                wavy: false,
            })
        } else {
            None
        },
        strikethrough: if run.strikethrough {
            Some(StrikethroughStyle {
                thickness: px(1.0),
                color: Some(run.color),
            })
        } else {
            None
        },
    }
}

#[inline]
pub fn default_gpui_run(len: usize, color: Hsla) -> TextRun {
    TextRun {
        len,
        font: Font {
            family: system_font_family(),
            weight: FontWeight::NORMAL,
            ..Default::default()
        },
        color,
        background_color: None,
        underline: None,
        strikethrough: None,
    }
}
