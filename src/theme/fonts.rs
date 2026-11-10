//! Font families on platforms where GPUI's defaults are not installed.
//!
//! GPUI resolves a family only by its exact name and, on Linux, maps
//! `.SystemUIFont` to IBM Plex Sans, which Echo does not bundle. Without an
//! installed family, chat, chrome, and ASCII-art text fall through GPUI's
//! generic fallback stack to whatever happens to be installed, and monospace
//! text turns proportional. Linux therefore picks installed families once at
//! startup, preferring the fonts configured in KDE Plasma.

use gpui::App;

/// Common Linux sans-serif families, the KDE Plasma default first.
#[cfg(target_os = "linux")]
const UI_CANDIDATES: &[&str] = &[
    "Noto Sans",
    "Adwaita Sans",
    "Cantarell",
    "Ubuntu",
    "DejaVu Sans",
    "Liberation Sans",
    "Roboto",
    "Open Sans",
    "Arial",
];

/// GNOME's own defaults, preferred inside a GNOME session.
#[cfg(target_os = "linux")]
const GNOME_UI_CANDIDATES: &[&str] = &["Adwaita Sans", "Cantarell"];

/// Monospace families for ASCII art: Echo's Windows family when installed,
/// then families with wide symbol and braille coverage.
#[cfg(target_os = "linux")]
const MONO_CANDIDATES: &[&str] = &[
    "Cascadia Code",
    "Cascadia Mono",
    "DejaVu Sans Mono",
    "Hack",
    "Noto Sans Mono",
    "Adwaita Mono",
    "JetBrains Mono",
    "Source Code Pro",
    "Fira Mono",
    "Liberation Mono",
    "Ubuntu Mono",
];

/// Install this platform's chat, chrome, and monospace families. Runs once,
/// before any text is laid out; other platforms keep the defaults.
#[cfg(target_os = "linux")]
pub fn install_platform_font_families(cx: &App) {
    use echo_core::freedesktop::{ini_value, qt_font_family};

    let installed: std::collections::HashSet<String> =
        cx.text_system().all_font_names().into_iter().collect();
    let kdeglobals = crate::storage_roots::external_config_dir()
        .and_then(|dir| std::fs::read_to_string(dir.join("kdeglobals")).ok())
        .unwrap_or_default();
    let configured = |key| ini_value(&kdeglobals, "General", key).and_then(qt_font_family);
    let gnome = std::env::var("XDG_CURRENT_DESKTOP").is_ok_and(|desktops| {
        desktops
            .split(':')
            .any(|desktop| desktop.eq_ignore_ascii_case("GNOME"))
    });
    let desktop_ui: &[&str] = if gnome { GNOME_UI_CANDIDATES } else { &[] };
    let pick = |configured: Option<&str>, candidates: &[&[&str]]| {
        configured
            .into_iter()
            .chain(candidates.iter().flat_map(|list| list.iter().copied()))
            .find(|family| installed.contains(*family))
            .map(str::to_owned)
    };

    let Some(ui) = pick(configured("font"), &[desktop_ui, UI_CANDIDATES]) else {
        crate::log_ui!(
            warn,
            "No common sans-serif font is installed; text uses GPUI's fallback fonts"
        );
        return;
    };
    let mono = pick(configured("fixed"), &[MONO_CANDIDATES]).unwrap_or_else(|| ui.clone());
    crate::log_ui!(info, ui = %ui, mono = %mono, "Using installed font families");
    if !echo_chat_render::text_run_intern::install_font_families(ui.into(), mono.into()) {
        crate::log_ui!(
            warn,
            "Font families were installed after text layout started; some text keeps the defaults"
        );
    }
}

#[cfg(not(target_os = "linux"))]
pub fn install_platform_font_families(_cx: &App) {}
