//! Settings storage and management.
#![allow(clippy::enum_variant_names)]

#[path = "highlight_matcher.rs"]
mod highlight_matcher;
mod secrets;

use arc_swap::ArcSwap;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
#[cfg(not(target_os = "windows"))]
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::LazyLock;
use std::sync::Mutex;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, AtomicU64, Ordering};
use tokio::sync::broadcast;

pub use super::embed_thumbnails::EmbedThumbnailMode;
pub use super::emote_quality::EmoteImageScale;
use super::hotkeys::HotkeyBinding;
use super::lock_utils::MutexExt;
pub use echo_text_core::EmojiStyle;

pub use echo_core::emote_display_rules::{
    CompiledEmoteDisplayRules, EmoteDisplayFlag, EmoteDisplayFlags, EmoteDisplayRule,
    EmoteRuleChange, EmoteRuleScope,
};
pub use echo_core::settings_event::SettingsEvent;

static SETTINGS_TX: OnceLock<broadcast::Sender<SettingsEvent>> = OnceLock::new();

/// PERF: Cached lowercase username for mention detection (avoids lock + clone + lowercase per message)
static CACHED_USERNAME_LOWERCASE: LazyLock<Mutex<String>> =
    LazyLock::new(|| Mutex::new(String::new()));

/// Version counter bumped whenever *layout-affecting* settings change.
///
/// This is intentionally separate from the broadcast channel: rendering code can cheaply poll this
/// atomic and invalidate layout caches even if the polling coordinator isn't running (or misses an
/// event), ensuring sliders like emote scale update live.
static LAYOUT_SETTINGS_VERSION: AtomicU64 = AtomicU64::new(0);

/// Bumped whenever a theme-affecting change is broadcast (theme selection,
/// custom palette edits, theme-file reloads, resolved OS theme flips, settings
/// import).
///
/// Theme lookups run for every painted color, so their caches gate on this
/// atomic instead of reading the theme string out of the settings mutex.
static THEME_SETTINGS_VERSION: AtomicU64 = AtomicU64::new(0);

/// Bumped whenever the filter rule set changes. The ignore-phrase replace pass
/// caches its compiled Replace rules keyed by this version so it only recompiles
/// regexes when the rules actually change (the hot message path reads it once
/// per message).
static FILTERS_VERSION: AtomicU64 = AtomicU64::new(1);

/// Debug counters for diagnosing "settings changed but UI didn't update" issues.
/// Only logged when the `ui` log category is enabled (e.g., `ECHO_LOG_UI=1`).
static SETTINGS_BROADCAST_COUNT: AtomicU64 = AtomicU64::new(0);
static LAST_SETTINGS_BROADCAST_LOG_MS: AtomicU64 = AtomicU64::new(0);
static CACHED_EMOTE_IMAGE_SCALE: AtomicU64 = AtomicU64::new(0);
static CACHED_WINDOW_TRANSPARENT: AtomicBool = AtomicBool::new(false);
/// Tray policy is read by the native Windows message window without taking the
/// settings mutex while it handles minimize and close requests.
static CACHED_MINIMIZE_TO_TRAY: AtomicBool = AtomicBool::new(false);
static CACHED_CLOSE_TO_TRAY: AtomicBool = AtomicBool::new(false);
/// Persisted anonymous-mode mirror. IRC/message admission reads this for every
/// incoming row, so it must not take the global settings mutex.
static CACHED_ANONYMOUS_MODE: AtomicBool = AtomicBool::new(false);
/// Hot-path mirrors for blocked-user and filter settings. Incoming chat reads
/// these atomics instead of taking the global settings mutex per message.
static CACHED_ENABLE_TWITCH_BLOCKED_USERS: AtomicBool = AtomicBool::new(true);
static CACHED_SHOW_BLOCKED_USERS_MESSAGES: AtomicU64 = AtomicU64::new(0);
static CACHED_EXCLUDE_USER_MESSAGES_FROM_FILTER: AtomicBool = AtomicBool::new(false);
/// Hot-path mirrors for chat pause policy. Pointer movement and message append
/// checks must not take the global settings mutex.
static CACHED_CHAT_HOVER_PAUSE_MODE: AtomicU8 = AtomicU8::new(ChatHoverPauseMode::Disabled as u8);
static CACHED_CHAT_PAUSE_MODIFIER: AtomicU8 = AtomicU8::new(ChatPauseModifier::Alt as u8);
static CACHED_USERNAME_RIGHT_CLICK_BEHAVIOR: AtomicU8 =
    AtomicU8::new(UsernameRightClickBehavior::Mention as u8);
static CACHED_USERNAME_RIGHT_CLICK_MODIFIER_BEHAVIOR: AtomicU8 =
    AtomicU8::new(UsernameRightClickBehavior::Reply as u8);
static CACHED_USERNAME_RIGHT_CLICK_MODIFIER: AtomicU8 =
    AtomicU8::new(UsernameRightClickModifier::Shift as u8);
static CACHED_SHOW_REPLY_BUTTON: AtomicBool = AtomicBool::new(false);
static CACHED_SHOW_LAST_READ_INDICATOR: AtomicBool = AtomicBool::new(false);
/// Hot-path mirrors for per-frame render reads (zebra stripes, pause
/// indicator). The render pass reads these once per split per frame and must
/// not take the global settings mutex.
static CACHED_ALTERNATE_MESSAGE_BACKGROUNDS: AtomicBool = AtomicBool::new(false);
static CACHED_SHOW_PAUSE_INDICATOR: AtomicBool = AtomicBool::new(true);
/// `f32` bits of the wheel line-scroll multiplier read on every wheel event.
static CACHED_MOUSE_SCROLL_MULTIPLIER: AtomicU32 = AtomicU32::new(0x3f80_0000);
static CACHED_SHOW_LIVE_CHATTER_INDICATOR: AtomicBool = AtomicBool::new(true);
/// Hot-path mirrors of message timestamp policy.
///
/// Only the two boolean policy bits live in atomics: the timestamp *shape* is a
/// user-authored token pattern, compiled once per change and published through
/// [`COMPILED_TIMESTAMP_FORMATTER`].
static CACHED_SHOW_MESSAGE_TIMESTAMPS: AtomicBool = AtomicBool::new(true);
static CACHED_HIDE_MESSAGE_TIMESTAMPS_WHEN_LIVE: AtomicBool = AtomicBool::new(false);
/// Main window device-pixel ratio (DPI scale), stored as `f32` bits.
///
/// Synced from the main window's render frames ([`set_window_scale_factor`]) and
/// read where no window is at hand (media preloads, emote picker, input preview)
/// to resolve the `Auto` emote image-scale tier. Chat layout uses its own
/// window's scale. Defaults to 1.0 until the first frame lands.
static CACHED_WINDOW_SCALE_FACTOR: AtomicU64 = AtomicU64::new(0x3f80_0000);

fn runtime_access_token(token: &str) -> Option<String> {
    if token.trim().is_empty() {
        None
    } else {
        Some(token.to_string())
    }
}

fn sync_top_level_account_credentials(settings: &mut AppSettings, account: &Account) {
    settings.username = Some(account.username.clone());
    settings.token = runtime_access_token(&account.token);
    settings.refresh_token = account.refresh_token.clone();
    if !account.client_id.trim().is_empty() {
        settings.client_id = account.client_id.clone();
    }
    settings.token_expires_at = account.token_expires_at;

    for existing in &mut settings.accounts {
        existing.is_active = existing.username.eq_ignore_ascii_case(&account.username);
    }
}

fn reconcile_active_account_credentials(settings: &mut AppSettings) {
    let top_username = settings
        .username
        .clone()
        .filter(|username| !username.trim().is_empty());
    let top_token = settings
        .token
        .clone()
        .filter(|token| !token.trim().is_empty());

    if let Some(active_index) = settings
        .accounts
        .iter()
        .position(|account| account.is_active)
    {
        if settings.accounts[active_index].token.trim().is_empty() {
            let active_username = settings.accounts[active_index].username.clone();
            if top_username
                .as_deref()
                .is_some_and(|username| username.eq_ignore_ascii_case(&active_username))
                && let Some(token) = top_token.clone()
            {
                settings.accounts[active_index].token = token;
                settings.accounts[active_index].refresh_token = settings.refresh_token.clone();
                if settings.accounts[active_index].client_id.trim().is_empty() {
                    settings.accounts[active_index].client_id = settings.client_id.clone();
                }
                settings.accounts[active_index].token_expires_at = settings.token_expires_at;
            }
        }

        if !settings.accounts[active_index].token.trim().is_empty() {
            let account = settings.accounts[active_index].clone();
            sync_top_level_account_credentials(settings, &account);
            return;
        }
    }

    if let (Some(username), Some(token)) = (top_username.clone(), top_token.clone()) {
        if let Some(account_index) = settings
            .accounts
            .iter()
            .position(|account| account.username.eq_ignore_ascii_case(&username))
        {
            settings.accounts[account_index].token = token;
            settings.accounts[account_index].refresh_token = settings.refresh_token.clone();
            if settings.accounts[account_index].client_id.trim().is_empty() {
                settings.accounts[account_index].client_id = settings.client_id.clone();
            }
            settings.accounts[account_index].token_expires_at = settings.token_expires_at;
            let account = settings.accounts[account_index].clone();
            sync_top_level_account_credentials(settings, &account);
            return;
        }

        let account = Account {
            username,
            display_name: None,
            profile_image_url: None,
            token,
            refresh_token: settings.refresh_token.clone(),
            client_id: settings.client_id.clone(),
            is_active: true,
            token_expires_at: settings.token_expires_at,
        };
        settings.accounts.push(account.clone());
        sync_top_level_account_credentials(settings, &account);
        return;
    }

    if let Some(account_index) = settings
        .accounts
        .iter()
        .position(|account| !account.token.trim().is_empty())
    {
        let account = settings.accounts[account_index].clone();
        sync_top_level_account_credentials(settings, &account);
    }
}

fn normalize_optional_path(value: Option<String>) -> Option<String> {
    value.and_then(|raw| {
        let trimmed = raw.trim().trim_matches('"');
        (!trimmed.is_empty()).then(|| trimmed.to_string())
    })
}

/// A UNC share (`\\server\share`, `//server/share`) or a device/NT path
/// (`\\?\`, `\\.\`, `\??\`). Opening one makes Windows authenticate to the
/// remote host (leaking an NTLM hash) or reach raw devices, so path setters
/// refuse new ones. Imports never apply path settings at all (see
/// [`keep_current_sensitive_settings`]), and a path the user already saved,
/// such as a NAS share, keeps working.
pub(crate) fn is_remote_or_device_path(path: &str) -> bool {
    let path = path.trim().trim_matches('"');
    let mut chars = path.chars();
    let is_separator = |c: Option<char>| matches!(c, Some('\\' | '/'));
    is_separator(chars.next()) && (is_separator(chars.next()) || path.starts_with("\\??\\"))
}

fn normalize_log_format_value(value: &str) -> String {
    match value
        .trim()
        .trim_start_matches('.')
        .to_ascii_lowercase()
        .as_str()
    {
        "log" => "log".to_string(),
        _ => "txt".to_string(),
    }
}

fn normalize_deleted_log_char_limit_value(value: usize) -> usize {
    value.clamp(100, 5000)
}

fn clear_theme_file_state(settings: &mut AppSettings) {
    settings.custom_theme_file_path.clear();
    settings.custom_theme_file_palette = None;
    settings.custom_theme_file_accent = None;
    settings.custom_theme_file_loaded_mtime_ms = None;
}

fn reconcile_theme_file_settings(settings: &mut AppSettings) {
    let Some(theme) = settings.theme.as_deref() else {
        clear_theme_file_state(settings);
        return;
    };
    if !theme.eq_ignore_ascii_case("theme file") {
        return;
    }

    let Some(path) = normalize_optional_path(Some(settings.custom_theme_file_path.clone())) else {
        tracing::warn!("Theme File mode had no path configured; falling back to Dark theme");
        clear_theme_file_state(settings);
        settings.theme = Some("Dark".to_string());
        return;
    };

    match crate::backend::theme_files::parse_theme_file(std::path::Path::new(&path)) {
        Ok(parsed) => {
            settings.custom_theme_file_path = path;
            settings.custom_theme_file_accent = parsed.accent.clone();
            settings.custom_theme_file_palette = Some(parsed.into());
            settings.custom_theme_file_loaded_mtime_ms =
                theme_file_mtime_ms(std::path::Path::new(&settings.custom_theme_file_path));
            if let Some(accent) = settings.custom_theme_file_accent.clone() {
                settings.primary_accent_color = accent;
            }
        }
        Err(error) => {
            tracing::warn!(
                "Failed to restore theme file '{}': {}; falling back to Dark theme",
                path,
                error
            );
            clear_theme_file_state(settings);
            settings.theme = Some("Dark".to_string());
        }
    }
}

fn theme_uses_system(theme: Option<&str>) -> bool {
    theme.is_some_and(|theme| theme.eq_ignore_ascii_case("system"))
}

#[cfg(not(target_os = "windows"))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SystemThemeProbeSource {
    Env(&'static str),
    File(&'static str),
    Unsupported(&'static str),
}

#[cfg(not(target_os = "windows"))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SystemThemeProbe {
    theme: ResolvedSystemTheme,
    source: SystemThemeProbeSource,
}

#[cfg(not(target_os = "windows"))]
impl SystemThemeProbe {
    const fn unavailable(reason: &'static str) -> Self {
        Self {
            theme: ResolvedSystemTheme::Unknown,
            source: SystemThemeProbeSource::Unsupported(reason),
        }
    }

    fn status_message(self) -> String {
        match (self.theme, self.source) {
            (ResolvedSystemTheme::Light, SystemThemeProbeSource::Env(source)) => {
                format!("Following the OS light theme via {source}")
            }
            (ResolvedSystemTheme::Dark, SystemThemeProbeSource::Env(source)) => {
                format!("Following the OS dark theme via {source}")
            }
            (ResolvedSystemTheme::Light, SystemThemeProbeSource::File(source)) => {
                format!("Following the OS light theme from {source}")
            }
            (ResolvedSystemTheme::Dark, SystemThemeProbeSource::File(source)) => {
                format!("Following the OS dark theme from {source}")
            }
            (_, SystemThemeProbeSource::Unsupported(reason)) => {
                format!("OS theme detection unavailable ({reason}); using the dark fallback")
            }
            (ResolvedSystemTheme::Unknown, SystemThemeProbeSource::Env(source)) => {
                format!("OS theme detection unavailable via {source}; using the dark fallback")
            }
            (ResolvedSystemTheme::Unknown, SystemThemeProbeSource::File(source)) => {
                format!("OS theme detection unavailable from {source}; using the dark fallback")
            }
        }
    }
}

#[cfg(not(target_os = "windows"))]
fn parse_prefers_dark_flag(value: &str) -> Option<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" | "dark" | "prefer-dark" => Some(true),
        "0" | "false" | "no" | "off" | "light" | "default" | "prefer-light" => Some(false),
        _ => None,
    }
}

#[cfg(not(target_os = "windows"))]
fn theme_from_bool(value: bool) -> ResolvedSystemTheme {
    if value {
        ResolvedSystemTheme::Dark
    } else {
        ResolvedSystemTheme::Light
    }
}

#[cfg(not(target_os = "windows"))]
fn theme_from_name_hint(value: &str) -> Option<ResolvedSystemTheme> {
    let normalized = value.trim().to_ascii_lowercase();
    if normalized.is_empty() {
        return None;
    }

    if normalized.contains(":dark")
        || normalized.ends_with("-dark")
        || normalized.ends_with("_dark")
        || normalized.contains("dark")
        || normalized.contains("night")
        || normalized.contains("black")
    {
        return Some(ResolvedSystemTheme::Dark);
    }

    if normalized.contains(":light")
        || normalized.ends_with("-light")
        || normalized.ends_with("_light")
        || normalized.contains("light")
        || normalized.contains("day")
        || normalized.contains("white")
    {
        return Some(ResolvedSystemTheme::Light);
    }

    None
}

#[cfg(not(target_os = "windows"))]
fn env_theme_probe(var: &'static str) -> Option<SystemThemeProbe> {
    let value = std::env::var(var).ok()?;
    if let Some(prefers_dark) = parse_prefers_dark_flag(&value) {
        return Some(SystemThemeProbe {
            theme: theme_from_bool(prefers_dark),
            source: SystemThemeProbeSource::Env(var),
        });
    }

    theme_from_name_hint(&value).map(|theme| SystemThemeProbe {
        theme,
        source: SystemThemeProbeSource::Env(var),
    })
}

#[cfg(not(target_os = "windows"))]
fn read_text_file(path: &Path) -> Option<String> {
    fs::read_to_string(path).ok()
}

#[cfg(not(target_os = "windows"))]
fn find_config_file(candidates: &[PathBuf]) -> Option<PathBuf> {
    candidates.iter().find(|path| path.is_file()).cloned()
}

#[cfg(any(
    target_os = "linux",
    target_os = "freebsd",
    target_os = "dragonfly",
    target_os = "netbsd",
    target_os = "openbsd"
))]
fn config_home() -> Option<PathBuf> {
    if crate::benchmark_profile::is_enabled() {
        return None;
    }
    std::env::var_os("XDG_CONFIG_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|home| home.join(".config")))
}

#[cfg(any(
    target_os = "linux",
    target_os = "freebsd",
    target_os = "dragonfly",
    target_os = "netbsd",
    target_os = "openbsd"
))]
fn gtk_settings_probe(path: &Path) -> Option<SystemThemeProbe> {
    let content = read_text_file(path)?;
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with('[') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        let value = value.trim();

        match key {
            "gtk-application-prefer-dark-theme" => {
                if let Some(prefers_dark) = parse_prefers_dark_flag(value) {
                    return Some(SystemThemeProbe {
                        theme: theme_from_bool(prefers_dark),
                        source: SystemThemeProbeSource::File("GTK settings.ini"),
                    });
                }
            }
            "gtk-theme-name" | "gtk-theme" | "gtk-color-scheme" | "color-scheme" => {
                if let Some(theme) = theme_from_name_hint(value) {
                    return Some(SystemThemeProbe {
                        theme,
                        source: SystemThemeProbeSource::File("GTK settings.ini"),
                    });
                }
                if let Some(prefers_dark) = parse_prefers_dark_flag(value) {
                    return Some(SystemThemeProbe {
                        theme: theme_from_bool(prefers_dark),
                        source: SystemThemeProbeSource::File("GTK settings.ini"),
                    });
                }
            }
            _ => {}
        }
    }
    None
}

#[cfg(any(
    target_os = "linux",
    target_os = "freebsd",
    target_os = "dragonfly",
    target_os = "netbsd",
    target_os = "openbsd"
))]
fn kde_globals_probe(path: &Path) -> Option<SystemThemeProbe> {
    let content = read_text_file(path)?;
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with('[') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        if matches!(
            key.trim(),
            "ColorScheme" | "LookAndFeelPackage" | "widgetStyle"
        ) && let Some(theme) = theme_from_name_hint(value.trim())
        {
            return Some(SystemThemeProbe {
                theme,
                source: SystemThemeProbeSource::File("kdeglobals"),
            });
        }
    }
    None
}

#[cfg(target_os = "macos")]
fn detect_resolved_system_theme_probe() -> SystemThemeProbe {
    if crate::benchmark_profile::is_enabled() {
        return SystemThemeProbe::unavailable("benchmark profile disables OS theme discovery");
    }
    if let Some(probe) = env_theme_probe("APPLE_INTERFACE_STYLE") {
        return probe;
    }

    let Some(home) = dirs::home_dir() else {
        return SystemThemeProbe::unavailable("HOME is not set");
    };

    let path = home.join("Library/Preferences/.GlobalPreferences.plist");
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(_) => return SystemThemeProbe::unavailable("macOS preferences are unreadable"),
    };

    if bytes
        .windows(b"AppleInterfaceStyle".len())
        .any(|w| w == b"AppleInterfaceStyle")
        && bytes.windows(b"Dark".len()).any(|w| w == b"Dark")
    {
        return SystemThemeProbe {
            theme: ResolvedSystemTheme::Dark,
            source: SystemThemeProbeSource::File("macOS global preferences"),
        };
    }

    SystemThemeProbe {
        theme: ResolvedSystemTheme::Light,
        source: SystemThemeProbeSource::File("macOS global preferences"),
    }
}

#[cfg(any(
    target_os = "linux",
    target_os = "freebsd",
    target_os = "dragonfly",
    target_os = "netbsd",
    target_os = "openbsd"
))]
fn detect_resolved_system_theme_probe() -> SystemThemeProbe {
    for var in [
        "ECHO_SYSTEM_THEME",
        "GTK_THEME",
        "GTK_APPLICATION_PREFER_DARK_THEME",
        "COLORSCHEME",
    ] {
        if let Some(probe) = env_theme_probe(var) {
            return probe;
        }
    }

    let Some(config_home) = config_home() else {
        return SystemThemeProbe::unavailable("config directory is unavailable");
    };

    let gtk_candidates = [
        config_home.join("gtk-4.0/settings.ini"),
        config_home.join("gtk-3.0/settings.ini"),
        dirs::home_dir()
            .map(|home| home.join(".gtkrc-2.0"))
            .unwrap_or_default(),
    ];
    if let Some(path) = find_config_file(&gtk_candidates)
        && let Some(probe) = gtk_settings_probe(&path)
    {
        return probe;
    }

    let kdeglobals = config_home.join("kdeglobals");
    if kdeglobals.is_file()
        && let Some(probe) = kde_globals_probe(&kdeglobals)
    {
        return probe;
    }

    SystemThemeProbe::unavailable("no supported desktop theme hint was found")
}

#[cfg(all(
    not(target_os = "windows"),
    not(target_os = "macos"),
    not(target_os = "linux"),
    not(target_os = "freebsd"),
    not(target_os = "dragonfly"),
    not(target_os = "netbsd"),
    not(target_os = "openbsd")
))]
fn detect_resolved_system_theme_probe() -> SystemThemeProbe {
    SystemThemeProbe::unavailable("this platform has no theme probe yet")
}

#[cfg(target_os = "windows")]
fn detect_resolved_system_theme() -> ResolvedSystemTheme {
    use windows::Win32::Foundation::ERROR_SUCCESS;
    use windows::Win32::System::Registry::{
        HKEY_CURRENT_USER, REG_VALUE_TYPE, RRF_RT_REG_DWORD, RegGetValueW,
    };
    use windows::core::w;

    let mut value = 0u32;
    let mut value_type = REG_VALUE_TYPE(0);
    let mut value_size = std::mem::size_of::<u32>() as u32;
    // SAFETY: the key and value names are static wide strings, and the out-pointers reference locals sized for a DWORD.
    let result = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            w!("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize"),
            w!("AppsUseLightTheme"),
            RRF_RT_REG_DWORD,
            Some(&mut value_type),
            Some((&mut value as *mut u32).cast()),
            Some(&mut value_size),
        )
    };

    if result == ERROR_SUCCESS {
        if value == 0 {
            ResolvedSystemTheme::Dark
        } else {
            ResolvedSystemTheme::Light
        }
    } else {
        ResolvedSystemTheme::Unknown
    }
}

#[cfg(not(target_os = "windows"))]
fn detect_resolved_system_theme() -> ResolvedSystemTheme {
    detect_resolved_system_theme_probe().theme
}

fn normalize_runtime_settings(settings: &mut AppSettings) {
    if settings.emote_picker_animation_policy_revision
        < CURRENT_EMOTE_PICKER_ANIMATION_POLICY_REVISION
    {
        if settings.emote_picker_animation_mode == EmotePickerAnimationMode::Hover {
            settings.emote_picker_animation_mode = EmotePickerAnimationMode::Always;
        }
        settings.emote_picker_animation_policy_revision =
            CURRENT_EMOTE_PICKER_ANIMATION_POLICY_REVISION;
    }
    settings.log_directory = if crate::benchmark_profile::is_enabled() {
        None
    } else {
        normalize_optional_path(settings.log_directory.take())
    };
    settings.log_format = normalize_log_format_value(&settings.log_format);
    settings.deleted_log_char_limit =
        normalize_deleted_log_char_limit_value(settings.deleted_log_char_limit);
    normalize_trusted_link_domains(settings);
    normalize_chat_popup_window_descriptors(settings);
    normalize_message_timestamp_pattern(settings);
    echo_core::emote_display_rules::normalize_rules(&mut settings.emote_display_rules);
    reconcile_theme_file_settings(settings);
    normalize_image_uploader_placeholder_key(settings);
    if theme_uses_system(settings.theme.as_deref())
        || settings.resolved_system_theme == ResolvedSystemTheme::Unknown
    {
        settings.resolved_system_theme = detect_resolved_system_theme();
    }
}

fn normalize_image_uploader_placeholder_key(settings: &mut AppSettings) {
    if settings.image_uploader.enabled
        && crate::backend::image_uploader::has_placeholder_uploader_key(&settings.image_uploader)
    {
        settings.image_uploader.enabled = false;
        tracing::warn!(
            "Disabled image uploader because its configuration contains a placeholder API key"
        );
    }
}

/// Emit at most one diagnostic about a rejected timestamp pattern per process.
///
/// A malformed pattern is a settings-file problem, not a per-render event: a
/// repeated warning would spam every load, import, and format edit.
fn warn_invalid_timestamp_pattern(err: TimestampPatternError) {
    static WARNED: std::sync::Once = std::sync::Once::new();
    WARNED.call_once(|| {
        tracing::warn!(
            "Ignoring invalid message timestamp pattern ({}); falling back to the migrated \
             legacy format or Echo's default.",
            err.message()
        );
    });
}

/// Resolve the authoritative timestamp pattern for a freshly loaded or
/// imported settings snapshot.
///
/// Order: a valid saved pattern wins; otherwise the legacy enum field is
/// migrated; otherwise the default `Hour24 : Minute` pattern is used. The
/// legacy field is cleared afterwards so only the new representation is saved.
fn normalize_message_timestamp_pattern(settings: &mut AppSettings) {
    let saved = settings.message_timestamp_pattern.take();
    let resolved = match saved {
        Some(pattern) => match pattern.validate() {
            Ok(()) => pattern,
            Err(err) => {
                warn_invalid_timestamp_pattern(err);
                migrate_legacy_timestamp_format(settings.message_timestamp_format)
            }
        },
        None => migrate_legacy_timestamp_format(settings.message_timestamp_format),
    };

    settings.message_timestamp_pattern = Some(resolved);
    settings.message_timestamp_format = None;
}

/// Migrate the legacy enum field, falling back to the default pattern.
fn migrate_legacy_timestamp_format(legacy: Option<MessageTimestampFormat>) -> TimestampPattern {
    let Some(legacy) = legacy else {
        return TimestampPattern::default();
    };
    let migrated = legacy.into_pattern();
    match migrated.validate() {
        Ok(()) => migrated,
        Err(err) => {
            warn_invalid_timestamp_pattern(err);
            TimestampPattern::default()
        }
    }
}

fn normalize_trusted_link_domains(settings: &mut AppSettings) {
    let mut normalized = Vec::with_capacity(settings.trusted_link_domains.len().min(128));
    for host in std::mem::take(&mut settings.trusted_link_domains) {
        let host = host.trim().trim_end_matches('.').to_ascii_lowercase();
        if host.is_empty()
            || host.len() > 253
            || host.contains('/')
            || host.contains(':')
            || normalized.iter().any(|saved| saved == &host)
        {
            continue;
        }
        normalized.push(host);
        if normalized.len() == 128 {
            break;
        }
    }
    settings.trusted_link_domains = normalized;
}

fn normalize_chat_popup_window_descriptors(settings: &mut AppSettings) {
    let mut normalized = Vec::with_capacity(
        settings
            .chat_popup_windows
            .len()
            .min(MAX_CLOSED_CHAT_POPUP_WINDOW_DESCRIPTORS),
    );

    for mut descriptor in std::mem::take(&mut settings.chat_popup_windows) {
        let channel = normalize_chat_popup_channel(&descriptor.channel);
        if channel.is_empty() {
            continue;
        }

        descriptor.channel = channel;
        descriptor.width = descriptor.width.clamp(360, 3840);
        descriptor.height = descriptor.height.clamp(420, 2160);
        descriptor.x = descriptor.x.clamp(-100_000, 100_000);
        descriptor.y = descriptor.y.clamp(-100_000, 100_000);

        if let Some(index) = normalized
            .iter()
            .position(|existing: &ChatPopupWindowDescriptor| existing.channel == descriptor.channel)
        {
            normalized.remove(index);
        }
        normalized.push(descriptor);
        trim_chat_popup_window_descriptors(&mut normalized);
    }

    settings.chat_popup_windows = normalized;
}

fn scrub_tokens_for_disk(settings: &AppSettings) -> AppSettings {
    let mut scrubbed = settings.clone();
    scrubbed.token = None;
    scrubbed.refresh_token = None;
    for account in &mut scrubbed.accounts {
        account.token.clear();
        account.refresh_token = None;
    }
    scrubbed
}

/// Settings export copy: tokens removed as on disk, plus uploader header values
/// (API keys) and proxy credentials, which normal persistence keeps.
fn scrub_secrets_for_export(settings: &AppSettings) -> AppSettings {
    let mut scrubbed = scrub_tokens_for_disk(settings);
    scrubbed.image_uploader.extra_headers =
        crate::backend::image_uploader::redact_extra_header_values(
            &scrubbed.image_uploader.extra_headers,
        );
    scrubbed.image_uploader.request_url = crate::backend::image_uploader::redact_url_query_values(
        &scrubbed.image_uploader.request_url,
    );
    scrubbed.proxy_url = strip_proxy_userinfo(&scrubbed.proxy_url);
    scrubbed
}

/// Remove `user:pass@` from a proxy URL, with or without a scheme.
fn strip_proxy_userinfo(proxy_url: &str) -> String {
    let Some((prefix, host)) = proxy_url.rsplit_once('@') else {
        return proxy_url.to_string();
    };
    match prefix.split_once("://") {
        Some((scheme, _)) => format!("{scheme}://{host}"),
        None => host.to_string(),
    }
}

/// JSON keys of settings that pick an executable or its arguments. A shared
/// settings file must not set them: import keeps the current values and the
/// legacy settings.json migration drops them.
const LAUNCH_SETTING_KEYS: &[&str] = &[
    "browser_path",
    "open_links_incognito",
    "custom_player_path",
    "use_custom_player",
    "streamlink_path",
    "streamlink_quality",
    "streamlink_custom_quality",
    "streamlink_extra_args",
];

/// Keep the current sensitive settings over imported ones: what Echo
/// executes, which files it reads or writes (log folder, theme file, highlight
/// sounds), and where it sends data (image uploader, proxy). Returns the labels
/// of the settings whose imported value differed and was therefore skipped.
fn keep_current_sensitive_settings(
    imported: &mut AppSettings,
    current: &AppSettings,
) -> Vec<&'static str> {
    let mut skipped = Vec::new();
    macro_rules! keep {
        ($($field:ident).+, $label:literal) => {
            if imported.$($field).+ != current.$($field).+ {
                skipped.push($label);
                imported.$($field).+ = current.$($field).+.clone();
            }
        };
    }
    keep!(browser_path, "custom browser");
    keep!(open_links_incognito, "open links in incognito");
    keep!(custom_player_path, "custom player");
    keep!(use_custom_player, "use custom player");
    keep!(streamlink_path, "Streamlink path");
    keep!(streamlink_quality, "Streamlink quality");
    keep!(streamlink_custom_quality, "Streamlink custom quality");
    keep!(streamlink_extra_args, "Streamlink extra arguments");
    keep!(chat_logging_enabled, "chat logging");
    keep!(log_directory, "log directory");
    if imported.custom_theme_file_path != current.custom_theme_file_path {
        skipped.push("theme file");
        imported.custom_theme_file_path = current.custom_theme_file_path.clone();
        let imported_uses_theme_file = imported
            .theme
            .as_deref()
            .is_some_and(|theme| theme.eq_ignore_ascii_case("theme file"));
        if imported_uses_theme_file {
            imported.theme = current.theme.clone();
        }
    }
    let mut sounds_changed = false;
    for rule in &mut imported.highlight_rules {
        let kept = current
            .highlight_rules
            .iter()
            .find(|existing| existing.id == rule.id)
            .and_then(|existing| existing.sound_path.clone());
        sounds_changed |= rule.sound_path != kept;
        rule.sound_path = kept;
    }
    if sounds_changed {
        skipped.push("highlight sound files");
    }
    keep!(image_uploader.enabled, "image uploader enabled");
    keep!(image_uploader.paste_upload_enabled, "image upload on paste");
    keep!(image_uploader.request_url, "image uploader URL");
    keep!(image_uploader.form_field, "image uploader form field");
    keep!(image_uploader.extra_headers, "image uploader headers");
    keep!(proxy_enabled, "proxy enabled");
    keep!(proxy_url, "proxy URL");
    skipped
}

/// Legacy settings.json content with the launch settings removed, or `None`
/// when it is not a JSON object.
fn strip_launch_settings_from_json(json: &str) -> Option<String> {
    let mut value: serde_json::Value = serde_json::from_str(json).ok()?;
    let object = value.as_object_mut()?;
    for key in LAUNCH_SETTING_KEYS {
        object.remove(*key);
    }
    serde_json::to_string_pretty(&value).ok()
}

/// Initialize the settings broadcast channel and username cache. Call once at app startup.
pub fn init_settings_channel() {
    if SETTINGS_TX.get().is_none() {
        let (tx, _) = broadcast::channel(64);
        let _ = SETTINGS_TX.set(tx);
    }
    let settings = SETTINGS.lock_or_recover();
    update_username_cache(settings.username.as_deref());
}

/// Subscribe to settings change events. Returns a receiver for the broadcast channel.
pub fn subscribe_settings() -> Option<broadcast::Receiver<SettingsEvent>> {
    SETTINGS_TX.get().map(|tx| tx.subscribe())
}

/// Current layout settings version (monotonic).
///
/// This must be cheap to read from hot paths (painting/layout). Use this to detect when layout
/// caches need to be invalidated due to settings changes (font scale, emote scale, density, etc.).
#[inline]
pub fn get_layout_settings_version() -> u64 {
    LAYOUT_SETTINGS_VERSION.load(Ordering::Acquire)
}

/// Current theme settings version (monotonic).
///
/// Bumped by every `SettingsEvent::ThemeChanged` broadcast, so theme caches can
/// key on it without locking the settings mutex per color lookup.
#[inline]
pub fn get_theme_settings_version() -> u64 {
    THEME_SETTINGS_VERSION.load(Ordering::Acquire)
}

/// Broadcast a settings change event to all subscribers.
/// Also wakes the frame rate controller so the polling coordinator processes the event
/// immediately, even if the main window is in idle/dormant mode (e.g., settings popout focused).
pub fn broadcast_settings_event(event: SettingsEvent) {
    if matches!(event, SettingsEvent::WindowAppearanceChanged) {
        crate::state::ui_invalidation::request(
            crate::state::ui_invalidation::UiInvalidation::WindowState,
        );
    }
    if matches!(event, SettingsEvent::ThemeChanged) {
        THEME_SETTINGS_VERSION.fetch_add(1, Ordering::Release);
    }
    let count = SETTINGS_BROADCAST_COUNT.fetch_add(1, Ordering::Relaxed) + 1;
    if let Some(tx) = SETTINGS_TX.get() {
        let _ = tx.send(event.clone());
    }

    let now = crate::utils::unix_now_ms();
    let last = LAST_SETTINGS_BROADCAST_LOG_MS.load(Ordering::Relaxed);
    if now.saturating_sub(last) >= 500
        && LAST_SETTINGS_BROADCAST_LOG_MS
            .compare_exchange(last, now, Ordering::Relaxed, Ordering::Relaxed)
            .is_ok()
    {
        crate::log_ui!(
            debug,
            count,
            layout_settings_version = get_layout_settings_version(),
            "Broadcast settings event: {:?}",
            event
        );
    }

    crate::managers::frame_rate_controller::record_activity();
    crate::managers::frame_rate_controller::wakeup();
}

/// Bump the chat layout settings version, recording the caller for
/// `ECHO_RELAYOUT_TRACE`.
#[track_caller]
fn bump_layout_settings_version() {
    crate::components::chat::canvas_renderer::relayout_trace::note_layout_invalidation(
        "settings_version",
        0,
    );
    LAYOUT_SETTINGS_VERSION.fetch_add(1, Ordering::AcqRel);
}

const STREAMER_LAYOUT_UNKNOWN: u8 = u8::MAX;
static LAST_STREAMER_LAYOUT_STATE: std::sync::atomic::AtomicU8 =
    std::sync::atomic::AtomicU8::new(STREAMER_LAYOUT_UNKNOWN);

/// Streamer mode only reaches chat layout through hidden link URLs and
/// thumbnails, so other streamer-mode changes keep resident layouts.
fn streamer_layout_state() -> u8 {
    u8::from(crate::backend::streamer_mode::should_hide_link_urls())
        | (u8::from(crate::backend::streamer_mode::should_hide_link_thumbnails()) << 1)
}

pub fn broadcast_streamer_mode_changed() {
    let state = streamer_layout_state();
    if LAST_STREAMER_LAYOUT_STATE.swap(state, Ordering::AcqRel) != state {
        bump_layout_settings_version();
    }
    request_top_bar_state_refresh();
    broadcast_settings_event(SettingsEvent::StreamerModeChanged);
}

pub fn broadcast_theme_changed() {
    bump_layout_settings_version();
    broadcast_settings_event(SettingsEvent::ThemeChanged);
}

/// Notify renderers that locally overridden username colors changed.
pub fn broadcast_username_color_overrides_changed() {
    bump_layout_settings_version();
    broadcast_settings_event(SettingsEvent::CosmeticSettingsChanged);
}

/// Default accent color matching SolidJS
pub const DEFAULT_ACCENT_COLOR: &str = "#7f86ff";

/// Embed display mode options
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub enum EmbedDisplayMode {
    #[serde(rename = "none")]
    None,
    #[default]
    #[serde(rename = "click")]
    Click,
    #[serde(rename = "always")]
    Always,
}

/// Per-provider opt-ins for rich previews. All providers remain enabled by
/// default so this refines, rather than changes, the existing global mode.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EmbedProviderSettings {
    #[serde(default = "default_true")]
    pub images: bool,
    #[serde(default = "default_true")]
    pub seventv: bool,
    #[serde(default = "default_true")]
    pub spotify: bool,
    #[serde(default = "default_true")]
    pub steam: bool,
    #[serde(default = "default_true")]
    pub twitch: bool,
    #[serde(default = "default_true")]
    pub youtube: bool,
    #[serde(default = "default_true")]
    pub twitter: bool,
    #[serde(default = "default_true")]
    pub reddit: bool,
    #[serde(default = "default_true")]
    pub generic: bool,
}

impl Default for EmbedProviderSettings {
    fn default() -> Self {
        Self {
            images: true,
            seventv: true,
            spotify: true,
            steam: true,
            twitch: true,
            youtube: true,
            twitter: true,
            reddit: true,
            generic: true,
        }
    }
}

/// Per-content opt-ins for rich previews. Kept independent from providers so
/// later resolver work has one stable scheduling-policy seam.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EmbedContentSettings {
    #[serde(default = "default_true")]
    pub images: bool,
    #[serde(default = "default_true")]
    pub clips: bool,
    #[serde(default = "default_true")]
    pub videos: bool,
    #[serde(default = "default_true")]
    pub posts: bool,
    #[serde(default = "default_true")]
    pub generic: bool,
}

impl Default for EmbedContentSettings {
    fn default() -> Self {
        Self {
            images: true,
            clips: true,
            videos: true,
            posts: true,
            generic: true,
        }
    }
}

/// Tab theme options
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub enum TabTheme {
    #[default]
    #[serde(rename = "chrome")]
    Chrome,
    #[serde(rename = "firefox")]
    Firefox,
    #[serde(rename = "edge")]
    Edge,
    #[serde(rename = "square")]
    Square,
    #[serde(rename = "arc")]
    Arc,
    // Existing settings files store this variant under its original key.
    #[serde(rename = "chatterino7")]
    Minimal,
}

/// Tab overflow mode options - how tabs behave when they don't fit
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub enum TabOverflowMode {
    #[serde(rename = "multiline")]
    MultiLine,
    #[serde(rename = "dropdown")]
    Dropdown,
    #[default]
    #[serde(rename = "scroll")]
    Scroll,
}

/// Tab placement options - whether single-row tabs share the title bar.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub enum TabPlacement {
    #[default]
    #[serde(rename = "dedicated")]
    Dedicated,
    #[serde(rename = "inline")]
    Inline,
}

/// Height options for the main custom titlebar. The serde default preserves
/// the pre-setting 28px chrome for existing profiles.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
pub enum TitlebarDensity {
    #[serde(rename = "standard")]
    Standard,
    #[default]
    #[serde(rename = "compact")]
    Compact,
}

impl TitlebarDensity {
    pub const fn height_px(self) -> f32 {
        match self {
            Self::Standard => crate::config::TITLE_BAR_STANDARD_HEIGHT_PX,
            Self::Compact => crate::config::TITLE_BAR_COMPACT_HEIGHT_PX,
        }
    }
}

/// Badge display mode options
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub enum BadgeDisplayMode {
    #[default]
    #[serde(rename = "number")]
    Number,
    #[serde(rename = "dot")]
    Dot,
    #[serde(rename = "highlight")]
    Highlight,
}

const CURRENT_EMOTE_PICKER_ANIMATION_POLICY_REVISION: u8 = 1;

/// Emote picker animation mode options
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
pub enum EmotePickerAnimationMode {
    #[default]
    #[serde(rename = "always")]
    Always,
    #[serde(rename = "hover")]
    Hover,
    #[serde(rename = "never")]
    Never,
}

impl EmotePickerAnimationMode {
    /// Whether an animated picker cell should actively advance frames.
    pub const fn animates_cell(self, hovered: bool) -> bool {
        match self {
            Self::Always => true,
            Self::Hover => hovered,
            Self::Never => false,
        }
    }
}

pub use echo_text_core::timestamp::*;

/// Monotonic compile counter. Every published formatter carries the value it
/// was compiled with, so derived caches can key on it directly.
static TIMESTAMP_FORMATTER_GENERATION: AtomicU64 = AtomicU64::new(0);

/// Currently published compiled formatter.
static COMPILED_TIMESTAMP_FORMATTER: LazyLock<ArcSwap<CompiledTimestampFormatter>> =
    LazyLock::new(|| {
        let generation = TIMESTAMP_FORMATTER_GENERATION.fetch_add(1, Ordering::AcqRel) + 1;
        ArcSwap::from_pointee(TimestampPattern::default().compile(generation))
    });

/// Compile `pattern` and publish it as the active formatter.
///
/// Only settings load, settings import, and an explicit format change reach
/// this; message rendering never compiles.
fn publish_compiled_timestamp_formatter(pattern: &TimestampPattern) {
    let generation = TIMESTAMP_FORMATTER_GENERATION.fetch_add(1, Ordering::AcqRel) + 1;
    COMPILED_TIMESTAMP_FORMATTER.store(Arc::new(pattern.compile(generation)));
}

/// Load the active compiled formatter (one `arc-swap` pointer read).
///
/// The returned snapshot carries its own `generation`, which is what derived
/// caches key on: reading the formatter and its generation together makes it
/// impossible to pair a cached string or reserved column width with a
/// different formatter than the one that produced it.
#[inline]
pub fn message_timestamp_formatter() -> Arc<CompiledTimestampFormatter> {
    COMPILED_TIMESTAMP_FORMATTER.load_full()
}

/// Modifier-only hold gesture for intentionally pausing chat bottom-follow.
///
/// This is separate from ordinary hotkeys because Echo's hotkey actions are
/// edge-triggered and require a non-modifier key; pause needs press and release
/// state. Exact matching also avoids treating Ctrl+Alt split actions as Alt.
#[repr(u8)]
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
pub enum ChatPauseModifier {
    #[serde(rename = "disabled")]
    Disabled = 0,
    #[default]
    #[serde(rename = "alt")]
    Alt = 1,
    #[serde(rename = "control")]
    Control = 2,
    #[serde(rename = "shift")]
    Shift = 3,
}

impl ChatPauseModifier {
    fn from_u8(value: u8) -> Self {
        match value {
            1 => Self::Alt,
            2 => Self::Control,
            3 => Self::Shift,
            _ => Self::Disabled,
        }
    }

    /// Match one configured modifier without accepting extra modifiers.
    pub const fn matches(self, control: bool, alt: bool, shift: bool, platform: bool) -> bool {
        match self {
            Self::Disabled => false,
            Self::Alt => alt && !control && !shift && !platform,
            Self::Control => control && !alt && !shift && !platform,
            Self::Shift => shift && !control && !alt && !platform,
        }
    }
}

/// Action taken when a chat username is right-clicked.
#[repr(u8)]
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum UsernameRightClickBehavior {
    Reply = 0,
    #[default]
    Mention = 1,
    Ignore = 2,
}

impl UsernameRightClickBehavior {
    fn from_u8(value: u8) -> Self {
        match value {
            0 => Self::Reply,
            2 => Self::Ignore,
            _ => Self::Mention,
        }
    }

    pub const fn settings_value(self) -> &'static str {
        match self {
            Self::Reply => "reply",
            Self::Mention => "mention",
            Self::Ignore => "ignore",
        }
    }
}

const fn default_username_right_click_modifier_behavior() -> UsernameRightClickBehavior {
    UsernameRightClickBehavior::Reply
}

/// Exact modifier chord which selects the alternate username right-click action.
#[repr(u8)]
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum UsernameRightClickModifier {
    #[default]
    Shift = 0,
    Control = 1,
    Alt = 2,
    Platform = 3,
}

impl UsernameRightClickModifier {
    fn from_u8(value: u8) -> Self {
        match value {
            1 => Self::Control,
            2 => Self::Alt,
            3 => Self::Platform,
            _ => Self::Shift,
        }
    }

    pub const fn settings_value(self) -> &'static str {
        match self {
            Self::Shift => "shift",
            Self::Control => "control",
            Self::Alt => "alt",
            Self::Platform => "platform",
        }
    }

    /// Compares the complete modifier set, so Ctrl+Shift must not
    /// accidentally trigger a setting configured for Shift alone.
    pub const fn matches(
        self,
        control: bool,
        alt: bool,
        shift: bool,
        platform: bool,
        function: bool,
    ) -> bool {
        if function {
            return false;
        }
        match self {
            Self::Shift => shift && !control && !alt && !platform,
            Self::Control => control && !alt && !shift && !platform,
            Self::Alt => alt && !control && !shift && !platform,
            Self::Platform => platform && !control && !alt && !shift,
        }
    }
}

/// Optional mouse-driven pause policy. Disabled is deliberately the default so
/// moving the pointer across chat never changes bottom-follow by itself.
#[repr(u8)]
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
pub enum ChatHoverPauseMode {
    #[default]
    #[serde(rename = "disabled")]
    Disabled = 0,
    #[serde(rename = "2s")]
    Seconds2 = 1,
    #[serde(rename = "3s")]
    Seconds3 = 2,
    #[serde(rename = "5s")]
    Seconds5 = 3,
    #[serde(rename = "10s")]
    Seconds10 = 4,
    #[serde(rename = "indefinite")]
    Indefinite = 5,
}

impl ChatHoverPauseMode {
    fn from_u8(value: u8) -> Self {
        match value {
            1 => Self::Seconds2,
            2 => Self::Seconds3,
            3 => Self::Seconds5,
            4 => Self::Seconds10,
            5 => Self::Indefinite,
            _ => Self::Disabled,
        }
    }

    pub const fn duration_secs(self) -> Option<u64> {
        match self {
            Self::Seconds2 => Some(2),
            Self::Seconds3 => Some(3),
            Self::Seconds5 => Some(5),
            Self::Seconds10 => Some(10),
            Self::Disabled | Self::Indefinite => None,
        }
    }
}

/// Chat density mode for message spacing
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Default)]
pub enum ChatDensity {
    #[serde(rename = "compact")]
    Compact,
    #[default]
    #[serde(rename = "normal")]
    Normal,
    #[serde(rename = "cozy")]
    Cozy,
}

/// How Twitch login and localized display names are presented in chat.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
pub enum UsernameDisplayMode {
    #[serde(rename = "username")]
    Username,
    #[serde(rename = "localized_name")]
    LocalizedName,
    #[default]
    #[serde(rename = "username_and_localized_name")]
    UsernameAndLocalizedName,
}

impl UsernameDisplayMode {
    pub const fn settings_value(self) -> &'static str {
        match self {
            Self::Username => "username",
            Self::LocalizedName => "localized_name",
            Self::UsernameAndLocalizedName => "username_and_localized_name",
        }
    }
}

/// When messages from Twitch-blocked users remain visible in a channel.
///
/// Blocked users stay hidden everywhere by default, or can remain visible
/// while the active account moderates/broadcasts the channel.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum BlockedUserVisibility {
    #[default]
    Never,
    IfModerator,
    IfBroadcaster,
}

impl BlockedUserVisibility {
    const fn as_u64(self) -> u64 {
        match self {
            Self::Never => 0,
            Self::IfModerator => 1,
            Self::IfBroadcaster => 2,
        }
    }

    const fn from_u64(value: u64) -> Self {
        match value {
            1 => Self::IfModerator,
            2 => Self::IfBroadcaster,
            _ => Self::Never,
        }
    }
}

impl ChatDensity {
    /// Get the line height multiplier for this density
    pub fn multiplier(&self) -> f32 {
        match self {
            ChatDensity::Compact => 0.9,
            ChatDensity::Normal => 1.0,
            ChatDensity::Cozy => 1.3,
        }
    }
}

/// Streamlink quality presets (Phase 7.4)
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum StreamlinkQuality {
    #[default]
    Best,
    P720_60,
    P480,
    AudioOnly,
    Custom,
}

/// Theme mode options
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum ThemeMode {
    #[default]
    Dark,
    Light,
    System,
    Oled,
    Midnight,
}

/// Controls whether enabled settings switches use the neutral track palette or
/// derive their track from the current accent color.
///
/// This remains serde-defaulted so settings files written before the option
/// retain the established neutral switch appearance.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum SwitchOnStyle {
    Accent,
    #[default]
    #[serde(other)]
    Neutral,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum ResolvedSystemTheme {
    #[default]
    Unknown,
    Light,
    Dark,
}

impl ResolvedSystemTheme {
    pub fn prefers_light(self) -> Option<bool> {
        match self {
            Self::Unknown => None,
            Self::Light => Some(true),
            Self::Dark => Some(false),
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Unknown => "Unavailable",
            Self::Light => "Light",
            Self::Dark => "Dark",
        }
    }
}

pub use echo_core::message::DeletedMessageMode;

/// Account data for multi-account support
#[derive(Clone, Serialize, Deserialize, PartialEq)]
pub struct Account {
    pub username: String,
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub profile_image_url: Option<String>,
    #[serde(default)]
    pub token: String,
    #[serde(default)]
    pub refresh_token: Option<String>,
    #[serde(default)]
    pub client_id: String,
    #[serde(default)]
    pub is_active: bool,
    #[serde(default)]
    pub token_expires_at: Option<i64>,
}

impl std::fmt::Debug for Account {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Account")
            .field("username", &self.username)
            .field("display_name", &self.display_name)
            .field("profile_image_url", &self.profile_image_url)
            .field("token", &crate::log::Secret(()))
            .field(
                "refresh_token",
                &self.refresh_token.as_ref().map(|_| crate::log::Secret(())),
            )
            .field("client_id", &self.client_id)
            .field("is_active", &self.is_active)
            .field("token_expires_at", &self.token_expires_at)
            .finish()
    }
}

/// Custom theme - simplified with auto-generated palette from base color
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CustomTheme {
    /// Base color for palette generation (hex #RRGGBB)
    #[serde(default = "default_base_color")]
    pub base_color: String,

    /// Whether to generate a light or dark palette
    #[serde(default)]
    pub is_light: bool,

    /// Saturation adjustment (0.0 = grayscale, 1.0 = full saturation)
    #[serde(default = "default_saturation")]
    pub saturation: f32,
}

fn default_base_color() -> String {
    "#6366f1".to_string()
}
fn default_saturation() -> f32 {
    0.15
}

impl Default for CustomTheme {
    fn default() -> Self {
        Self {
            base_color: default_base_color(),
            is_light: false,
            saturation: default_saturation(),
        }
    }
}

impl CustomTheme {
    /// Generate the full color palette from the base color
    pub fn generate_palette(&self) -> GeneratedPalette {
        let (h, _, _) = hex_to_hsl(&self.base_color).unwrap_or((0.65, 0.7, 0.6));
        let sat = self.saturation * 3.0;

        if self.is_light {
            GeneratedPalette {
                bg_void: hsl_to_hex(h, sat * 0.5, 0.88),
                bg_surface: hsl_to_hex(h, sat * 0.35, 0.95),
                bg_plate: hsl_to_hex(h, sat * 0.45, 0.90),
                bg_elevated: hsl_to_hex(h, sat * 0.25, 0.97),
                bg_hover: hsl_to_hex(h, sat * 0.55, 0.85),
                text_primary: hsl_to_hex(h, sat * 0.4, 0.15),
                text_secondary: hsl_to_hex(h, sat * 0.3, 0.35),
                text_muted: hsl_to_hex(h, sat * 0.2, 0.50),
                border: hsl_to_hex(h, sat * 0.35, 0.75),
                border_strong: hsl_to_hex(h, sat * 0.45, 0.60),
            }
        } else {
            GeneratedPalette {
                bg_void: hsl_to_hex(h, sat * 0.5, 0.02),
                bg_surface: hsl_to_hex(h, sat * 0.4, 0.06),
                bg_plate: hsl_to_hex(h, sat * 0.45, 0.04),
                bg_elevated: hsl_to_hex(h, sat * 0.35, 0.08),
                bg_hover: hsl_to_hex(h, sat * 0.3, 0.12),
                text_primary: hsl_to_hex(h, sat * 0.1, 0.90),
                text_secondary: hsl_to_hex(h, sat * 0.08, 0.63),
                text_muted: hsl_to_hex(h, sat * 0.05, 0.44),
                border: hsl_to_hex(h, sat * 0.25, 0.16),
                border_strong: hsl_to_hex(h, sat * 0.20, 0.26),
            }
        }
    }
}

/// Generated color palette from CustomTheme
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GeneratedPalette {
    pub bg_void: String,
    pub bg_surface: String,
    pub bg_plate: String,
    pub bg_elevated: String,
    pub bg_hover: String,
    pub text_primary: String,
    pub text_secondary: String,
    pub text_muted: String,
    pub border: String,
    pub border_strong: String,
}

impl From<crate::backend::theme_files::ThemeFilePalette> for GeneratedPalette {
    fn from(palette: crate::backend::theme_files::ThemeFilePalette) -> Self {
        Self {
            bg_void: palette.bg_void,
            bg_surface: palette.bg_surface,
            bg_plate: palette.bg_plate,
            bg_elevated: palette.bg_elevated,
            bg_hover: palette.bg_hover,
            text_primary: palette.text_primary,
            text_secondary: palette.text_secondary,
            text_muted: palette.text_muted,
            border: palette.border,
            border_strong: palette.border_strong,
        }
    }
}

/// Convert hex to HSL (h: 0-1, s: 0-1, l: 0-1)
fn hex_to_hsl(hex: &str) -> Option<(f32, f32, f32)> {
    let (r, g, b) = hex_to_rgb(hex)?;
    Some(rgb_to_hsl(r, g, b))
}

/// Convert 8-bit RGB channels to HSL (h: 0-1, s: 0-1, l: 0-1)
pub(crate) fn rgb_to_hsl(r: u8, g: u8, b: u8) -> (f32, f32, f32) {
    let r = r as f32 / 255.0;
    let g = g as f32 / 255.0;
    let b = b as f32 / 255.0;

    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let l = (max + min) / 2.0;

    if (max - min).abs() < f32::EPSILON {
        return (0.0, 0.0, l);
    }

    let d = max - min;
    let s = if l > 0.5 {
        d / (2.0 - max - min)
    } else {
        d / (max + min)
    };

    let h = if (max - r).abs() < f32::EPSILON {
        let mut h = (g - b) / d;
        if g < b {
            h += 6.0;
        }
        h / 6.0
    } else if (max - g).abs() < f32::EPSILON {
        ((b - r) / d + 2.0) / 6.0
    } else {
        ((r - g) / d + 4.0) / 6.0
    };

    (h, s, l)
}

/// Convert HSL to hex (h: 0-1, s: 0-1, l: 0-1)
fn hsl_to_hex(h: f32, s: f32, l: f32) -> String {
    let (r, g, b) = hsl_to_rgb(h, s, l);
    format!("#{:02x}{:02x}{:02x}", r, g, b)
}

/// Convert HSL to RGB
fn hsl_to_rgb(h: f32, s: f32, l: f32) -> (u8, u8, u8) {
    if s.abs() < f32::EPSILON {
        let v = (l * 255.0).round() as u8;
        return (v, v, v);
    }

    let q = if l < 0.5 {
        l * (1.0 + s)
    } else {
        l + s - l * s
    };
    let p = 2.0 * l - q;

    let r = hue_to_rgb(p, q, h + 1.0 / 3.0);
    let g = hue_to_rgb(p, q, h);
    let b = hue_to_rgb(p, q, h - 1.0 / 3.0);

    (
        (r * 255.0).round() as u8,
        (g * 255.0).round() as u8,
        (b * 255.0).round() as u8,
    )
}

fn hue_to_rgb(p: f32, q: f32, mut t: f32) -> f32 {
    if t < 0.0 {
        t += 1.0;
    }
    if t > 1.0 {
        t -= 1.0;
    }
    if t < 1.0 / 6.0 {
        return p + (q - p) * 6.0 * t;
    }
    if t < 1.0 / 2.0 {
        return q;
    }
    if t < 2.0 / 3.0 {
        return p + (q - p) * (2.0 / 3.0 - t) * 6.0;
    }
    p
}

const MAX_CLOSED_CHAT_POPUP_WINDOW_DESCRIPTORS: usize = 32;

/// Persisted geometry and open-state for one native channel popup.
///
/// Entries are normalized by channel and kept in least-to-most-recent order.
/// All open popups are retained, while closed history is bounded so placement
/// state cannot grow for every channel ever visited.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChatPopupWindowDescriptor {
    pub channel: String,
    #[serde(default = "default_chat_popup_window_width")]
    pub width: u32,
    #[serde(default = "default_chat_popup_window_height")]
    pub height: u32,
    #[serde(default)]
    pub has_position: bool,
    #[serde(default)]
    pub x: i32,
    #[serde(default)]
    pub y: i32,
    #[serde(default)]
    pub is_open: bool,
    #[serde(default)]
    pub is_maximized: bool,
}

/// Geometry remembered for the emote picker on one physical display.
///
/// Uses GPUI's platform display UUID instead of a session-local display ID.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EmotePickerPopoutDisplayGeometry {
    pub display_uuid: String,
    #[serde(default = "default_emote_picker_popout_window_width")]
    pub width: u32,
    #[serde(default = "default_emote_picker_popout_window_height")]
    pub height: u32,
    #[serde(default)]
    pub has_position: bool,
    #[serde(default)]
    pub x: i32,
    #[serde(default)]
    pub y: i32,
}

/// A settings destination remembered across sessions.
///
/// Stored by slug, never by enum index, so renaming a Rust variant or
/// reordering the navigation tables cannot invalidate a user's position. The
/// typed <-> slug conversion (and the fallback rules for slugs that no longer
/// exist) live in `components::settings::settings_navigation`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct PersistedSettingsLocation {
    /// Page slug, e.g. `"appearance"`.
    #[serde(default)]
    pub section: String,
    /// Subsection (scroll anchor) slug, e.g. `"theme-colors"`.
    #[serde(default)]
    pub subsection: Option<String>,
}

/// Persisted settings-navigation state.
///
/// Additive and serde-defaulted: an older settings file simply deserializes to
/// `version: 0`, which the UI reads as "never persisted" and answers with the
/// first-open defaults (Account selected, General expanded). Queries, focused
/// nodes, active search results, and scroll offsets are deliberately
/// session-only.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct SettingsNavigationPrefs {
    /// `0` means "never written"; mutators stamp
    /// [`SETTINGS_NAVIGATION_PREFS_VERSION`].
    #[serde(default)]
    pub version: u32,
    /// Last location outside the developer disclosure.
    #[serde(default)]
    pub last_standard: Option<PersistedSettingsLocation>,
    /// Last developer-only location, restored when the disclosure is turned
    /// back on.
    #[serde(default)]
    pub last_developer: Option<PersistedSettingsLocation>,
    /// Explicitly expanded category/page node keys. Search-forced expansion is
    /// temporary and never written here.
    #[serde(default)]
    pub expanded_nodes: Vec<String>,
}

/// Current schema revision for [`SettingsNavigationPrefs`].
pub const SETTINGS_NAVIGATION_PREFS_VERSION: u32 = 1;

impl SettingsNavigationPrefs {
    /// Whether the user has ever navigated with the new tree. `false` selects
    /// the first-open defaults instead of an empty expansion set.
    pub fn is_initialized(&self) -> bool {
        self.version >= SETTINGS_NAVIGATION_PREFS_VERSION
    }
}

#[derive(Clone, Serialize, Deserialize, PartialEq)]
pub struct AppSettings {
    pub username: Option<String>,
    pub token: Option<String>,
    pub refresh_token: Option<String>,
    #[serde(default)]
    pub client_id: String,
    #[serde(default)]
    pub channels: Vec<String>,

    /// Recently visited channels for predictive emote preloading (max 5)
    #[serde(default)]
    pub recent_channels: Vec<String>,

    #[serde(default)]
    pub theme: Option<String>,
    #[serde(default)]
    pub resolved_system_theme: ResolvedSystemTheme,
    #[serde(default = "default_accent_color")]
    pub primary_accent_color: String,
    /// Whether enabled switch tracks follow the accent or stay neutral.
    #[serde(default)]
    pub switch_on_style: SwitchOnStyle,
    /// Optional user override for primary application/chat text.
    /// `None` keeps the active theme or theme-file palette in control.
    #[serde(default)]
    pub primary_text_color: Option<String>,
    /// Custom theme colors (used when theme == "Custom")
    #[serde(default)]
    pub custom_theme: CustomTheme,
    #[serde(default)]
    pub custom_theme_file_path: String,
    #[serde(default)]
    pub custom_theme_file_auto_reload: bool,
    #[serde(default)]
    pub custom_theme_file_palette: Option<GeneratedPalette>,
    #[serde(default)]
    pub custom_theme_file_accent: Option<String>,
    #[serde(default)]
    pub custom_theme_file_loaded_mtime_ms: Option<i64>,
    /// Theme mode (Dark, Light, System, OLED, Midnight)
    #[serde(default)]
    pub theme_mode: ThemeMode,

    #[serde(default = "default_scale")]
    pub font_scale: f32,
    #[serde(default = "default_scale")]
    pub emote_scale: f32,
    #[serde(default)]
    pub emote_image_scale: EmoteImageScale,
    #[serde(default = "default_badge_scale")]
    pub badge_scale: f32,
    #[serde(default)]
    pub remove_emote_spacing: bool,
    #[serde(default = "default_true")]
    pub zero_width_enabled: bool,
    #[serde(default = "default_true")]
    pub enable_seventv_emotes: bool,
    #[serde(default = "default_true")]
    pub enable_bttv_emotes: bool,
    #[serde(default = "default_true")]
    pub enable_ffz_emotes: bool,

    #[serde(default)]
    pub embed_display_mode: EmbedDisplayMode,
    #[serde(default)]
    pub embed_providers: EmbedProviderSettings,
    #[serde(default)]
    pub embed_content: EmbedContentSettings,
    #[serde(default)]
    pub embed_remote_fetch_enabled: bool,
    #[serde(default)]
    pub embed_thumbnail_mode: EmbedThumbnailMode,

    #[serde(default)]
    pub tab_theme: TabTheme,
    #[serde(default)]
    pub tab_overflow_mode: TabOverflowMode,
    #[serde(default)]
    pub tab_placement: TabPlacement,
    #[serde(default)]
    pub titlebar_density: TitlebarDensity,

    #[serde(default)]
    pub badge_display_mode: BadgeDisplayMode,

    #[serde(default = "default_true")]
    pub tab_complete_usernames: bool,
    #[serde(default = "default_true")]
    pub mention_users_with_comma: bool,
    #[serde(default)]
    pub username_right_click_behavior: UsernameRightClickBehavior,
    #[serde(default = "default_username_right_click_modifier_behavior")]
    pub username_right_click_modifier_behavior: UsernameRightClickBehavior,
    #[serde(default)]
    pub username_right_click_modifier: UsernameRightClickModifier,
    #[serde(default)]
    pub show_reply_button: bool,
    /// Mark the last message seen before a tab or the window lost focus.
    #[serde(default)]
    pub show_last_read_indicator: bool,
    #[serde(default)]
    pub unified_input_mode: bool,
    #[serde(default)]
    pub compact_split_header: bool,
    #[serde(default)]
    pub show_send_button: bool,
    #[serde(default = "default_true")]
    pub slash_commands_enabled: bool,
    #[serde(default = "default_true")]
    pub command_autocomplete_enabled: bool,
    #[serde(default = "default_true")]
    pub show_command_descriptions: bool,

    #[serde(default)]
    pub emote_picker_animation_mode: EmotePickerAnimationMode,
    /// Revision 0 represents settings saved before Always became the default.
    #[serde(default)]
    pub emote_picker_animation_policy_revision: u8,
    #[serde(default)]
    pub hide_emote_picker_load_more_warning: bool,

    #[serde(default)]
    pub emoji_style: EmojiStyle,

    #[serde(default)]
    pub anonymous_mode: bool,

    #[serde(default = "default_true")]
    pub open_settings_as_popout: bool,

    #[serde(default)]
    pub accounts: Vec<Account>,
    #[serde(default)]
    pub token_expires_at: Option<i64>,

    #[serde(default = "default_mention_color")]
    pub mention_color: String,
    #[serde(default = "default_whisper_color")]
    pub whisper_color: String,
    #[serde(default = "default_subscription_color")]
    pub subscription_color: String,
    #[serde(default = "default_first_message_color")]
    pub first_message_color: String,
    #[serde(default = "default_thread_highlight_color")]
    pub thread_highlight_color: String,

    #[serde(default = "default_true")]
    pub flash_on_mention: bool,
    #[serde(default = "default_true")]
    pub show_mention_in_title: bool,
    #[serde(default = "default_true")]
    pub thread_highlight_enabled: bool,
    #[serde(default = "default_true")]
    pub thread_highlight_sound_enabled: bool,
    #[serde(default = "default_true")]
    pub thread_highlight_show_in_mentions: bool,

    #[serde(default)]
    pub filters: Vec<FilterRule>,
    #[serde(default = "default_true")]
    pub enable_twitch_blocked_users: bool,
    #[serde(default)]
    pub show_blocked_users_messages: BlockedUserVisibility,
    #[serde(default)]
    pub exclude_user_messages_from_filter: bool,
    #[serde(default)]
    pub highlight_blacklist_users: Vec<HighlightBlacklistUser>,
    #[serde(default)]
    pub highlight_rules: Vec<HighlightRule>,

    #[serde(default)]
    pub custom_commands: Vec<CustomCommand>,

    #[serde(default = "crate::backend::mod_buttons::default_mod_buttons")]
    pub mod_buttons: Vec<crate::backend::mod_buttons::ModButtonConfig>,

    #[serde(default)]
    pub developer_mode: bool,

    #[serde(default)]
    pub chat_logging_enabled: bool,
    /// Persist recent per-channel message history to disk for offline/gap-fill.
    #[serde(default = "default_persist_history_enabled")]
    pub persist_history_enabled: bool,
    #[serde(default)]
    pub log_directory: Option<String>,
    #[serde(default = "default_log_format")]
    pub log_format: String,

    #[serde(default)]
    pub proxy_enabled: bool,
    #[serde(default)]
    pub proxy_url: String,

    #[serde(default)]
    pub chat_density: ChatDensity,
    #[serde(default)]
    pub username_display_mode: UsernameDisplayMode,
    #[serde(default = "default_true")]
    pub colorize_users_without_color: bool,
    /// Show a compact LIVE marker for chatters currently streaming on Twitch.
    #[serde(default = "default_true")]
    pub show_live_chatter_indicator: bool,

    #[serde(default = "default_hotkeys")]
    pub hotkeys: HashMap<String, HotkeyBinding>,

    #[serde(default)]
    pub streamer_mode: Option<super::streamer_mode::StreamerModeSettings>,

    #[serde(default)]
    pub similarity: super::similarity::SimilaritySettings,

    /// Show system messages when users join or leave the channel
    #[serde(default)]
    pub show_join_part_messages: bool,

    /// Alternate the background of every other chat message (zebra striping)
    #[serde(default)]
    pub alternate_message_backgrounds: bool,
    /// Display message timestamps using the OS-local timezone.
    #[serde(default = "default_true")]
    pub show_message_timestamps: bool,
    /// Ordered timestamp token pattern; visibility is stored separately.
    ///
    /// `None` only until the first successful load migrates the legacy field.
    #[serde(default)]
    pub message_timestamp_pattern: Option<TimestampPattern>,
    /// Legacy timestamp-format field.
    ///
    /// It is read on load and migrated into `message_timestamp_pattern`, but
    /// never serialized again: Echo saves only the new representation.
    #[serde(default, skip_serializing)]
    pub message_timestamp_format: Option<MessageTimestampFormat>,
    /// Hide the timestamp column while the channel is live.
    #[serde(default)]
    pub hide_message_timestamps_when_live: bool,

    /// Local user renames applied to rendered display names
    #[serde(default)]
    pub nicknames: Vec<NicknameRule>,

    /// Per-emote presentation rules: show an emote's code as text, or show an
    /// animated emote's first frame, everywhere or in one channel. Read
    /// leniently: an unparseable rule is dropped rather than failing the file.
    #[serde(
        default,
        deserialize_with = "echo_core::emote_display_rules::deserialize_rules_lenient"
    )]
    pub emote_display_rules: Vec<EmoteDisplayRule>,

    /// Animate animated emotes in chat and related previews.
    #[serde(default = "default_true")]
    pub animate_emotes: bool,
    /// Render non-content interface motion statically. This intentionally does
    /// not change the user's animated-emote or picker playback policy.
    #[serde(default)]
    pub reduce_decorative_motion: bool,
    #[serde(default = "default_true")]
    pub highlight_flash_enabled: bool,
    #[serde(default = "default_true")]
    pub smooth_scroll_enabled: bool,
    /// Spread bursts of live messages over the gap until the next burst.
    #[serde(default = "default_true")]
    pub paced_chat_reveal: bool,
    /// Scroll animation duration in milliseconds (0 = instant, 50-500 typical)
    /// Default: 150ms
    #[serde(default = "default_scroll_animation_duration")]
    pub scroll_animation_duration_ms: u32,

    #[serde(default = "default_true")]
    pub show_ffz_badges: bool,
    #[serde(default = "default_true")]
    pub show_bttv_badges: bool,

    #[serde(default = "default_true")]
    pub show_bttv_pro_badges: bool,
    #[serde(default = "default_true")]
    pub show_ffz_bot_badges: bool,

    #[serde(default = "default_true")]
    pub show_seventv_name_paints: bool,
    #[serde(default = "default_true")]
    pub show_seventv_paint_shadows: bool,
    #[serde(default = "default_true")]
    pub show_seventv_cosmetic_badges: bool,
    #[serde(default = "default_true")]
    pub show_seventv_profile_avatars: bool,

    #[serde(default)]
    pub always_on_top: bool,
    #[serde(default)]
    pub window_transparent: bool,
    #[serde(default)]
    pub minimize_to_tray: bool,
    #[serde(default)]
    pub close_to_tray: bool,
    #[serde(default = "default_main_window_width")]
    pub main_window_width: u32,
    #[serde(default = "default_main_window_height")]
    pub main_window_height: u32,
    #[serde(default)]
    pub main_window_maximized: bool,
    #[serde(default)]
    pub main_window_has_position: bool,
    #[serde(default)]
    pub main_window_x: i32,
    #[serde(default)]
    pub main_window_y: i32,
    #[serde(default)]
    pub overlay_click_through: bool,
    #[serde(default)]
    pub overlay_ghost_mode: bool,
    #[serde(default = "default_overlay_window_width")]
    pub overlay_window_width: u32,
    #[serde(default = "default_overlay_window_height")]
    pub overlay_window_height: u32,
    #[serde(default)]
    pub overlay_window_has_position: bool,
    #[serde(default)]
    pub overlay_window_x: i32,
    #[serde(default)]
    pub overlay_window_y: i32,
    #[serde(default = "default_overlay_max_messages")]
    pub overlay_max_messages: u32,
    #[serde(default = "default_overlay_idle_fade_secs")]
    pub overlay_idle_fade_secs: u32,
    #[serde(default = "default_overlay_min_opacity_percent")]
    pub overlay_min_opacity_percent: u8,
    #[serde(default = "default_chat_popup_window_width")]
    pub chat_popup_window_width: u32,
    #[serde(default = "default_chat_popup_window_height")]
    pub chat_popup_window_height: u32,
    #[serde(default)]
    pub chat_popup_window_has_position: bool,
    #[serde(default)]
    pub chat_popup_window_x: i32,
    #[serde(default)]
    pub chat_popup_window_y: i32,
    #[serde(default)]
    pub chat_popup_windows: Vec<ChatPopupWindowDescriptor>,
    #[serde(default = "default_emote_picker_popout_window_width")]
    pub emote_picker_popout_window_width: u32,
    #[serde(default = "default_emote_picker_popout_window_height")]
    pub emote_picker_popout_window_height: u32,
    #[serde(default)]
    pub emote_picker_popout_window_has_position: bool,
    #[serde(default)]
    pub emote_picker_popout_window_x: i32,
    #[serde(default)]
    pub emote_picker_popout_window_y: i32,
    #[serde(default)]
    pub emote_picker_popout_display_geometries: Vec<EmotePickerPopoutDisplayGeometry>,
    #[serde(default = "default_user_card_popout_window_width")]
    pub user_card_popout_window_width: u32,
    #[serde(default = "default_user_card_popout_window_height")]
    pub user_card_popout_window_height: u32,
    #[serde(default)]
    pub user_card_popout_window_has_position: bool,
    #[serde(default)]
    pub user_card_popout_window_x: i32,
    #[serde(default)]
    pub user_card_popout_window_y: i32,
    #[serde(default = "default_settings_popout_window_width")]
    pub settings_popout_window_width: u32,
    #[serde(default = "default_settings_popout_window_height")]
    pub settings_popout_window_height: u32,
    #[serde(default)]
    pub settings_popout_window_has_position: bool,
    #[serde(default)]
    pub settings_popout_window_x: i32,
    #[serde(default)]
    pub settings_popout_window_y: i32,
    #[serde(default = "default_debug_popup_window_width")]
    pub debug_popup_window_width: u32,
    #[serde(default = "default_debug_popup_window_height")]
    pub debug_popup_window_height: u32,
    #[serde(default)]
    pub debug_popup_window_has_position: bool,
    #[serde(default)]
    pub debug_popup_window_x: i32,
    #[serde(default)]
    pub debug_popup_window_y: i32,

    #[serde(default)]
    pub chat_pause_modifier: ChatPauseModifier,
    #[serde(default)]
    pub chat_hover_pause_mode: ChatHoverPauseMode,
    #[serde(default = "default_true")]
    pub show_pause_indicator: bool,
    /// Scales mouse-wheel line scrolling.
    #[serde(default = "default_scale")]
    pub mouse_scroll_multiplier: f32,

    #[serde(default)]
    pub show_char_counter: bool,

    /// Confirm every pasted image before uploading it.
    #[serde(default = "default_true")]
    pub ask_before_paste_upload: bool,

    #[serde(default = "default_true")]
    pub collapse_ascii_art: bool,

    #[serde(default = "default_true")]
    pub show_pronouns: bool,
    #[serde(default = "default_true")]
    pub show_follow_date: bool,
    #[serde(default = "default_true")]
    pub show_follower_count: bool,

    #[serde(default = "default_watchstreak_highlight_color")]
    pub watchstreak_highlight_color: String,
    #[serde(default = "default_automod_highlight_color")]
    pub automod_highlight_color: String,
    #[serde(default = "default_announcement_highlight_color")]
    pub announcement_highlight_color: String,
    #[serde(default = "default_raid_highlight_color")]
    pub raid_highlight_color: String,
    #[serde(default = "default_bits_highlight_color")]
    pub bits_highlight_color: String,
    #[serde(default = "default_phrase_highlight_color")]
    pub phrase_highlight_color: String,
    #[serde(default = "default_redeem_highlight_color")]
    pub redeem_highlight_color: String,
    #[serde(default = "default_elevated_highlight_color")]
    pub elevated_highlight_color: String,
    #[serde(default = "default_self_message_highlight_color")]
    pub self_message_highlight_color: String,
    /// Highlight all of the local user's own messages.
    #[serde(default)]
    pub enable_self_message_highlight: bool,
    /// Tint paid/elevated and Hype Chat messages.
    #[serde(default = "default_true")]
    pub enable_elevated_highlight: bool,

    #[serde(default)]
    pub log_server_timestamps: bool,
    #[serde(default)]
    pub log_by_stream_id: bool,
    #[serde(default)]
    pub log_deleted_messages: bool,
    #[serde(default = "default_deleted_log_char_limit")]
    pub deleted_log_char_limit: usize,

    #[serde(default)]
    pub open_links_incognito: bool,
    /// Skip Echo's external-link confirmation for every HTTP(S) domain.
    #[serde(default)]
    pub skip_open_link_confirmation: bool,
    /// Exact, normalized HTTP(S) hosts that may open without confirmation.
    #[serde(default)]
    pub trusted_link_domains: Vec<String>,
    /// Check Echo's signed stable update manifest shortly after startup.
    #[serde(default = "default_true")]
    pub auto_check_updates: bool,
    #[serde(default)]
    pub browser_path: Option<String>,

    #[serde(default = "default_true")]
    pub live_notifications_enabled: bool,
    #[serde(default = "default_live_notification_startup_delay")]
    pub live_notification_startup_delay_secs: u32,

    #[serde(default)]
    pub custom_player_path: Option<String>,
    #[serde(default)]
    pub use_custom_player: bool,
    #[serde(default)]
    pub streamlink_path: Option<String>,
    #[serde(default)]
    pub streamlink_quality: StreamlinkQuality,
    #[serde(default)]
    pub streamlink_custom_quality: String,
    #[serde(default)]
    pub streamlink_extra_args: String,

    #[serde(default)]
    pub image_uploader: crate::backend::image_uploader::ImageUploaderConfig,

    #[serde(default)]
    pub deleted_message_mode: DeletedMessageMode,

    #[serde(default)]
    pub settings_navigation: SettingsNavigationPrefs,

    /// The first-run "Import from Chatterino" card was dismissed or an import
    /// ran, so it never shows again.
    #[serde(default)]
    pub chatterino_import_prompt_dismissed: bool,
}

/// Identity fields only: the full settings carry OAuth tokens, uploader API
/// keys and proxy credentials, and are too large to be useful in a log.
impl std::fmt::Debug for AppSettings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppSettings")
            .field("username", &self.username)
            .field("anonymous_mode", &self.anonymous_mode)
            .field("accounts", &self.accounts)
            .finish_non_exhaustive()
    }
}

/// Filter rule for message filtering
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FilterRule {
    pub id: String,
    pub expression: String,
    pub action: FilterAction,
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Replacement text used when `action == Replace`. For Replace rules
    /// `expression` is treated as a literal PATTERN (or regex when
    /// `pattern_is_regex` is set), not the boolean filter expression language.
    /// `None`/empty removes the matched text.
    #[serde(default)]
    pub replacement: Option<String>,
    /// When true, a Replace rule's `expression` is compiled as a regex instead
    /// of a case-insensitive literal substring.
    #[serde(default)]
    pub pattern_is_regex: bool,
}

/// User entries that suppress highlight side effects.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HighlightBlacklistUser {
    #[serde(default = "default_highlight_blacklist_id")]
    pub id: String,
    pub pattern: String,
    #[serde(default)]
    pub regex: bool,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

impl HighlightBlacklistUser {
    pub fn matches_username(&self, username: &str) -> bool {
        if !self.enabled {
            return false;
        }

        let pattern = self.pattern.trim();
        if pattern.is_empty() {
            return false;
        }

        if self.regex {
            return regex::RegexBuilder::new(pattern)
                .case_insensitive(true)
                .unicode(true)
                .build()
                .map(|regex| regex.is_match(username))
                .unwrap_or(false);
        }

        username.eq_ignore_ascii_case(pattern)
    }
}

fn default_highlight_blacklist_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum HighlightRuleTarget {
    #[default]
    Phrase,
    User,
    Badge,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HighlightRule {
    #[serde(default = "default_highlight_rule_id")]
    pub id: String,
    #[serde(default)]
    pub target: HighlightRuleTarget,
    pub pattern: String,
    #[serde(default)]
    pub regex: bool,
    #[serde(default)]
    pub case_sensitive: bool,
    #[serde(default = "default_true")]
    pub show_in_mentions: bool,
    #[serde(default)]
    pub play_sound: bool,
    #[serde(default)]
    pub alert: bool,
    #[serde(default)]
    pub sound_path: Option<String>,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub color: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MatchedHighlightRule {
    pub highlight_type: crate::backend::models::HighlightType,
    pub show_in_mentions: bool,
    pub play_sound: bool,
    pub alert: bool,
    pub sound_path: Option<String>,
}

impl HighlightRule {
    fn normalized_pattern(&self) -> Option<&str> {
        let pattern = self.pattern.trim();
        (!pattern.is_empty()).then_some(pattern)
    }
}

fn default_highlight_rule_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// Local user rename: replaces a sender's rendered display name at message
/// layout time. Purely cosmetic and local; the login keeps identifying the
/// user everywhere else.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct NicknameRule {
    #[serde(default = "default_nickname_rule_id")]
    pub id: String,
    /// Exact name to match, or a regex when `regex` is true.
    pub pattern: String,
    /// Replacement text. Regex rules support capture groups (`$1`, ...).
    pub replacement: String,
    /// Optional stable Twitch user ID. When present, this exact target wins
    /// before name-pattern/regex rules and is only used for rendered names.
    #[serde(default)]
    pub user_id: Option<String>,
    #[serde(default)]
    pub regex: bool,
    #[serde(default)]
    pub case_sensitive: bool,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

fn default_nickname_rule_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// Immutable nickname lookup data published when settings change.
///
/// The persisted [`NicknameRule`] list remains the source of truth for the
/// settings UI and serialization. Message layout reads this compact snapshot
/// so it never needs the settings mutex, a clone of the rule list, or a regex
/// compilation.
#[derive(Default)]
struct CompiledNicknameRules {
    /// First enabled stable-ID rule for each exact Twitch user ID. A present
    /// empty replacement is intentional: it remains a terminal match just as
    /// it is in the persisted-rule evaluator.
    stable_user_ids: HashMap<String, String>,
    /// Enabled non-ID rules in their persisted order; first match wins.
    name_rules: Vec<CompiledNicknameNameRule>,
}

enum CompiledNicknameNameRule {
    Exact {
        pattern: String,
        replacement: String,
        case_sensitive: bool,
    },
    Regex {
        regex: regex::Regex,
        replacement: String,
    },
}

impl CompiledNicknameRules {
    fn from_rules(rules: &[NicknameRule]) -> Self {
        let mut compiled = Self::default();

        for rule in rules {
            if !rule.enabled {
                continue;
            }

            if let Some(user_id) = rule.user_id.as_deref() {
                compiled
                    .stable_user_ids
                    .entry(user_id.to_owned())
                    .or_insert_with(|| rule.replacement.clone());
                continue;
            }

            let pattern = rule.pattern.trim();
            if pattern.is_empty() {
                continue;
            }

            if rule.regex {
                let Ok(regex) = regex::RegexBuilder::new(pattern)
                    .case_insensitive(!rule.case_sensitive)
                    .unicode(true)
                    .build()
                else {
                    continue;
                };
                compiled.name_rules.push(CompiledNicknameNameRule::Regex {
                    regex,
                    replacement: rule.replacement.clone(),
                });
            } else {
                compiled.name_rules.push(CompiledNicknameNameRule::Exact {
                    pattern: pattern.to_owned(),
                    replacement: rule.replacement.clone(),
                    case_sensitive: rule.case_sensitive,
                });
            }
        }

        compiled
    }

    fn apply(&self, login: &str, display_name: &str, user_id: Option<&str>) -> Option<String> {
        let stable_user_id = user_id.map(str::trim).filter(|user_id| !user_id.is_empty());
        if let Some(replacement) =
            stable_user_id.and_then(|user_id| self.stable_user_ids.get(user_id))
        {
            return (!replacement.trim().is_empty()).then(|| replacement.clone());
        }

        self.name_rules
            .iter()
            .find_map(|rule| match rule {
                CompiledNicknameNameRule::Exact {
                    pattern,
                    replacement,
                    case_sensitive,
                } => {
                    let matches = if *case_sensitive {
                        display_name == pattern || login == pattern
                    } else {
                        display_name.eq_ignore_ascii_case(pattern)
                            || login.eq_ignore_ascii_case(pattern)
                    };
                    matches.then(|| replacement.clone())
                }
                CompiledNicknameNameRule::Regex { regex, replacement } => {
                    if regex.is_match(display_name) {
                        Some(
                            regex
                                .replace_all(display_name, replacement.as_str())
                                .into_owned(),
                        )
                    } else if regex.is_match(login) {
                        Some(replacement.clone())
                    } else {
                        None
                    }
                }
            })
            .filter(|renamed| !renamed.trim().is_empty())
    }
}

/// Current nickname lookup snapshot. It starts empty so the settings loader
/// can publish the real rules without initializing `SETTINGS` recursively.
static COMPILED_NICKNAME_RULES: LazyLock<ArcSwap<CompiledNicknameRules>> =
    LazyLock::new(|| ArcSwap::from_pointee(CompiledNicknameRules::default()));
static COMPILED_NICKNAME_RULES_PUBLISHED: AtomicBool = AtomicBool::new(false);

fn publish_compiled_nickname_rules(rules: &[NicknameRule]) {
    COMPILED_NICKNAME_RULES.store(Arc::new(CompiledNicknameRules::from_rules(rules)));
    COMPILED_NICKNAME_RULES_PUBLISHED.store(true, Ordering::Release);
}

/// Resolve a sender's rendered display name through the enabled nickname
/// rules. First matching rule wins. The snapshot is compiled when settings
/// change, so layout performs one immutable pointer read with no settings lock
/// or regex compilation.
pub fn apply_nickname_rules(
    login: &str,
    display_name: &str,
    user_id: Option<&str>,
) -> Option<String> {
    if !COMPILED_NICKNAME_RULES_PUBLISHED.load(Ordering::Acquire) {
        LazyLock::force(&SETTINGS);
    }
    COMPILED_NICKNAME_RULES
        .load()
        .apply(login, display_name, user_id)
}

const EMOTE_DISPLAY_RULES_UNPUBLISHED: u8 = 0;
const EMOTE_DISPLAY_RULES_NONE: u8 = 1;
const EMOTE_DISPLAY_RULES_ACTIVE: u8 = 2;

/// Current emote display rule snapshot. Layout reads the state byte first, so
/// with no rules it never touches the snapshot pointer.
static COMPILED_EMOTE_DISPLAY_RULES: LazyLock<ArcSwap<CompiledEmoteDisplayRules>> =
    LazyLock::new(|| ArcSwap::from_pointee(CompiledEmoteDisplayRules::default()));
static EMOTE_DISPLAY_RULES_STATE: AtomicU8 = AtomicU8::new(EMOTE_DISPLAY_RULES_UNPUBLISHED);

fn publish_compiled_emote_display_rules(rules: &[EmoteDisplayRule]) {
    let compiled = CompiledEmoteDisplayRules::from_rules(rules);
    let state = if compiled.is_empty() {
        EMOTE_DISPLAY_RULES_NONE
    } else {
        EMOTE_DISPLAY_RULES_ACTIVE
    };
    COMPILED_EMOTE_DISPLAY_RULES.store(Arc::new(compiled));
    EMOTE_DISPLAY_RULES_STATE.store(state, Ordering::Release);
}

/// The active emote display rules, or `None` when there are none. Callers take
/// one snapshot per message or scan and probe it per emote.
#[inline]
pub fn emote_display_rules_snapshot() -> Option<Arc<CompiledEmoteDisplayRules>> {
    let mut state = EMOTE_DISPLAY_RULES_STATE.load(Ordering::Acquire);
    if state == EMOTE_DISPLAY_RULES_UNPUBLISHED {
        LazyLock::force(&SETTINGS);
        state = EMOTE_DISPLAY_RULES_STATE.load(Ordering::Acquire);
    }
    (state == EMOTE_DISPLAY_RULES_ACTIVE).then(|| COMPILED_EMOTE_DISPLAY_RULES.load_full())
}

/// Filter action types
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub enum FilterAction {
    #[default]
    #[serde(rename = "allow")]
    Allow,
    #[serde(rename = "hide")]
    Hide,
    #[serde(rename = "highlight")]
    Highlight,
    /// Rewrite the matched span of the message text with
    /// `FilterRule::replacement` at build time.
    #[serde(rename = "replace")]
    Replace,
}

pub use echo_core::commands::CustomCommand;

fn default_scale() -> f32 {
    1.0
}

fn default_badge_scale() -> f32 {
    1.0
}

fn default_true() -> bool {
    true
}

fn default_accent_color() -> String {
    DEFAULT_ACCENT_COLOR.to_string()
}

pub(crate) const DEFAULT_MENTION_COLOR: &str = "#7F3F49";
pub(crate) const DEFAULT_WHISPER_COLOR: &str = "#7F3F49";
pub(crate) const DEFAULT_SUBSCRIPTION_COLOR: &str = "#C466FF";
pub(crate) const DEFAULT_FIRST_MESSAGE_COLOR: &str = "#4CAF50";
pub(crate) const DEFAULT_THREAD_HIGHLIGHT_COLOR: &str = "#8f3018";
pub(crate) const DEFAULT_WATCHSTREAK_HIGHLIGHT_COLOR: &str = "#0082FF";
pub(crate) const DEFAULT_AUTOMOD_HIGHLIGHT_COLOR: &str = "#404040";
pub(crate) const DEFAULT_ANNOUNCEMENT_HIGHLIGHT_COLOR: &str = "#FF66ED";
pub(crate) const DEFAULT_RAID_HIGHLIGHT_COLOR: &str = "#9147FF";
pub(crate) const DEFAULT_BITS_HIGHLIGHT_COLOR: &str = "#FFD700";
pub(crate) const DEFAULT_PHRASE_HIGHLIGHT_COLOR: &str = "#7F3F49";
pub(crate) const DEFAULT_REDEEM_HIGHLIGHT_COLOR: &str = "#1C7E8D";
pub(crate) const DEFAULT_ELEVATED_HIGHLIGHT_COLOR: &str = "#FFAE42";
pub(crate) const DEFAULT_SELF_MESSAGE_HIGHLIGHT_COLOR: &str = "#0076DD";

fn default_mention_color() -> String {
    DEFAULT_MENTION_COLOR.to_string()
}

fn default_whisper_color() -> String {
    DEFAULT_WHISPER_COLOR.to_string()
}

fn default_subscription_color() -> String {
    DEFAULT_SUBSCRIPTION_COLOR.to_string()
}

fn default_first_message_color() -> String {
    DEFAULT_FIRST_MESSAGE_COLOR.to_string()
}

fn default_thread_highlight_color() -> String {
    DEFAULT_THREAD_HIGHLIGHT_COLOR.to_string()
}

fn default_log_format() -> String {
    "txt".to_string()
}

fn default_persist_history_enabled() -> bool {
    true
}

fn default_scroll_animation_duration() -> u32 {
    150
}

fn default_hotkeys() -> HashMap<String, HotkeyBinding> {
    super::hotkeys::default_hotkeys()
}

fn default_watchstreak_highlight_color() -> String {
    DEFAULT_WATCHSTREAK_HIGHLIGHT_COLOR.to_string()
}

fn default_automod_highlight_color() -> String {
    DEFAULT_AUTOMOD_HIGHLIGHT_COLOR.to_string()
}

fn default_announcement_highlight_color() -> String {
    DEFAULT_ANNOUNCEMENT_HIGHLIGHT_COLOR.to_string()
}

fn default_raid_highlight_color() -> String {
    DEFAULT_RAID_HIGHLIGHT_COLOR.to_string()
}

fn default_bits_highlight_color() -> String {
    DEFAULT_BITS_HIGHLIGHT_COLOR.to_string()
}

fn default_phrase_highlight_color() -> String {
    DEFAULT_PHRASE_HIGHLIGHT_COLOR.to_string()
}

fn default_redeem_highlight_color() -> String {
    DEFAULT_REDEEM_HIGHLIGHT_COLOR.to_string()
}

fn default_elevated_highlight_color() -> String {
    DEFAULT_ELEVATED_HIGHLIGHT_COLOR.to_string()
}

fn default_self_message_highlight_color() -> String {
    DEFAULT_SELF_MESSAGE_HIGHLIGHT_COLOR.to_string()
}

fn default_deleted_log_char_limit() -> usize {
    500
}

fn default_live_notification_startup_delay() -> u32 {
    30
}

fn default_main_window_width() -> u32 {
    1280
}

fn default_main_window_height() -> u32 {
    800
}

fn default_overlay_window_width() -> u32 {
    400
}

fn default_overlay_window_height() -> u32 {
    500
}

fn default_overlay_max_messages() -> u32 {
    15
}

fn default_overlay_idle_fade_secs() -> u32 {
    30
}

fn default_overlay_min_opacity_percent() -> u8 {
    10
}

fn default_chat_popup_window_width() -> u32 {
    520
}

fn default_chat_popup_window_height() -> u32 {
    720
}

fn default_emote_picker_popout_window_width() -> u32 {
    420
}

fn default_emote_picker_popout_window_height() -> u32 {
    520
}

fn default_settings_popout_window_width() -> u32 {
    850
}

fn default_settings_popout_window_height() -> u32 {
    650
}

fn default_debug_popup_window_width() -> u32 {
    720
}

fn default_debug_popup_window_height() -> u32 {
    820
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            username: None,
            token: None,
            refresh_token: None,
            client_id: String::new(),
            channels: Vec::new(),
            recent_channels: Vec::new(),
            theme: None,
            resolved_system_theme: ResolvedSystemTheme::Unknown,
            primary_accent_color: DEFAULT_ACCENT_COLOR.to_string(),
            switch_on_style: SwitchOnStyle::Neutral,
            primary_text_color: None,
            custom_theme: CustomTheme::default(),
            custom_theme_file_path: String::new(),
            custom_theme_file_auto_reload: false,
            custom_theme_file_palette: None,
            custom_theme_file_accent: None,
            custom_theme_file_loaded_mtime_ms: None,
            theme_mode: ThemeMode::Dark,
            font_scale: 1.0,
            emote_scale: 1.0,
            emote_image_scale: EmoteImageScale::default(),
            badge_scale: 1.0,
            remove_emote_spacing: false,
            zero_width_enabled: true,
            enable_seventv_emotes: true,
            enable_bttv_emotes: true,
            enable_ffz_emotes: true,
            embed_display_mode: EmbedDisplayMode::Click,
            embed_providers: EmbedProviderSettings::default(),
            embed_content: EmbedContentSettings::default(),
            embed_remote_fetch_enabled: false,
            embed_thumbnail_mode: EmbedThumbnailMode::default(),
            tab_theme: TabTheme::Chrome,
            tab_overflow_mode: TabOverflowMode::Scroll,
            tab_placement: TabPlacement::Dedicated,
            titlebar_density: TitlebarDensity::Compact,
            badge_display_mode: BadgeDisplayMode::Number,
            tab_complete_usernames: true,
            mention_users_with_comma: true,
            username_right_click_behavior: UsernameRightClickBehavior::Mention,
            username_right_click_modifier_behavior: UsernameRightClickBehavior::Reply,
            username_right_click_modifier: UsernameRightClickModifier::Shift,
            show_reply_button: false,
            show_last_read_indicator: false,
            unified_input_mode: false,
            compact_split_header: false,
            show_send_button: false,
            slash_commands_enabled: true,
            command_autocomplete_enabled: true,
            show_command_descriptions: true,
            emote_picker_animation_mode: EmotePickerAnimationMode::Always,
            emote_picker_animation_policy_revision: CURRENT_EMOTE_PICKER_ANIMATION_POLICY_REVISION,
            hide_emote_picker_load_more_warning: false,
            emoji_style: EmojiStyle::Twemoji,
            anonymous_mode: false,
            open_settings_as_popout: true,
            accounts: Vec::new(),
            token_expires_at: None,
            mention_color: default_mention_color(),
            whisper_color: default_whisper_color(),
            subscription_color: default_subscription_color(),
            first_message_color: default_first_message_color(),
            thread_highlight_color: default_thread_highlight_color(),
            flash_on_mention: true,
            show_mention_in_title: true,
            thread_highlight_enabled: true,
            thread_highlight_sound_enabled: true,
            thread_highlight_show_in_mentions: true,
            filters: Vec::new(),
            enable_twitch_blocked_users: true,
            show_blocked_users_messages: BlockedUserVisibility::Never,
            exclude_user_messages_from_filter: false,
            highlight_blacklist_users: Vec::new(),
            highlight_rules: Vec::new(),
            custom_commands: Vec::new(),
            mod_buttons: crate::backend::mod_buttons::default_mod_buttons(),
            developer_mode: false,
            chat_logging_enabled: false,
            persist_history_enabled: default_persist_history_enabled(),
            log_directory: None,
            log_format: default_log_format(),
            proxy_enabled: false,
            proxy_url: String::new(),
            chat_density: ChatDensity::Normal,
            username_display_mode: UsernameDisplayMode::default(),
            colorize_users_without_color: true,
            show_live_chatter_indicator: true,
            hotkeys: default_hotkeys(),
            streamer_mode: None,
            similarity: super::similarity::SimilaritySettings::default(),
            show_join_part_messages: false,
            alternate_message_backgrounds: false,
            show_message_timestamps: true,
            message_timestamp_pattern: Some(TimestampPattern::default()),
            message_timestamp_format: None,
            hide_message_timestamps_when_live: false,
            nicknames: Vec::new(),
            emote_display_rules: Vec::new(),
            reduce_decorative_motion: false,
            animate_emotes: true,
            highlight_flash_enabled: true,
            smooth_scroll_enabled: true,
            paced_chat_reveal: true,
            scroll_animation_duration_ms: default_scroll_animation_duration(),
            show_ffz_badges: true,
            show_bttv_badges: true,
            show_bttv_pro_badges: true,
            show_ffz_bot_badges: true,
            show_seventv_name_paints: true,
            show_seventv_paint_shadows: true,
            show_seventv_cosmetic_badges: true,
            show_seventv_profile_avatars: true,
            always_on_top: false,
            window_transparent: false,
            minimize_to_tray: false,
            close_to_tray: false,
            main_window_width: default_main_window_width(),
            main_window_height: default_main_window_height(),
            main_window_maximized: false,
            main_window_has_position: false,
            main_window_x: 0,
            main_window_y: 0,
            overlay_click_through: false,
            overlay_ghost_mode: false,
            overlay_window_width: default_overlay_window_width(),
            overlay_window_height: default_overlay_window_height(),
            overlay_window_has_position: false,
            overlay_window_x: 0,
            overlay_window_y: 0,
            overlay_max_messages: default_overlay_max_messages(),
            overlay_idle_fade_secs: default_overlay_idle_fade_secs(),
            overlay_min_opacity_percent: default_overlay_min_opacity_percent(),
            chat_popup_window_width: default_chat_popup_window_width(),
            chat_popup_window_height: default_chat_popup_window_height(),
            chat_popup_window_has_position: false,
            chat_popup_window_x: 0,
            chat_popup_window_y: 0,
            chat_popup_windows: Vec::new(),
            emote_picker_popout_window_width: default_emote_picker_popout_window_width(),
            emote_picker_popout_window_height: default_emote_picker_popout_window_height(),
            emote_picker_popout_window_has_position: false,
            emote_picker_popout_window_x: 0,
            emote_picker_popout_window_y: 0,
            emote_picker_popout_display_geometries: Vec::new(),
            user_card_popout_window_width: default_user_card_popout_window_width(),
            user_card_popout_window_height: default_user_card_popout_window_height(),
            user_card_popout_window_has_position: false,
            user_card_popout_window_x: 0,
            user_card_popout_window_y: 0,
            settings_popout_window_width: default_settings_popout_window_width(),
            settings_popout_window_height: default_settings_popout_window_height(),
            settings_popout_window_has_position: false,
            settings_popout_window_x: 0,
            settings_popout_window_y: 0,
            debug_popup_window_width: default_debug_popup_window_width(),
            debug_popup_window_height: default_debug_popup_window_height(),
            debug_popup_window_has_position: false,
            debug_popup_window_x: 0,
            debug_popup_window_y: 0,
            chat_pause_modifier: ChatPauseModifier::Alt,
            chat_hover_pause_mode: ChatHoverPauseMode::Disabled,
            show_pause_indicator: true,
            mouse_scroll_multiplier: 1.0,
            show_char_counter: false,
            ask_before_paste_upload: true,
            collapse_ascii_art: true,
            show_pronouns: true,
            show_follow_date: true,
            show_follower_count: true,
            watchstreak_highlight_color: default_watchstreak_highlight_color(),
            automod_highlight_color: default_automod_highlight_color(),
            announcement_highlight_color: default_announcement_highlight_color(),
            raid_highlight_color: default_raid_highlight_color(),
            bits_highlight_color: default_bits_highlight_color(),
            phrase_highlight_color: default_phrase_highlight_color(),
            redeem_highlight_color: default_redeem_highlight_color(),
            elevated_highlight_color: default_elevated_highlight_color(),
            self_message_highlight_color: default_self_message_highlight_color(),
            enable_self_message_highlight: false,
            enable_elevated_highlight: true,
            log_server_timestamps: false,
            log_by_stream_id: false,
            log_deleted_messages: false,
            deleted_log_char_limit: default_deleted_log_char_limit(),
            open_links_incognito: false,
            skip_open_link_confirmation: false,
            trusted_link_domains: Vec::new(),
            auto_check_updates: true,
            browser_path: None,
            live_notifications_enabled: true,
            live_notification_startup_delay_secs: default_live_notification_startup_delay(),
            custom_player_path: None,
            use_custom_player: false,
            streamlink_path: None,
            streamlink_quality: StreamlinkQuality::Best,
            streamlink_custom_quality: String::new(),
            streamlink_extra_args: String::new(),
            image_uploader: crate::backend::image_uploader::ImageUploaderConfig::default(),
            deleted_message_mode: DeletedMessageMode::Dim,
            settings_navigation: SettingsNavigationPrefs::default(),
            chatterino_import_prompt_dismissed: false,
        }
    }
}

static TOKEN_PERSISTENCE_WARNED: AtomicBool = AtomicBool::new(false);

fn report_token_persistence(result: Result<(), Vec<String>>) {
    let Err(failed_keys) = result else {
        TOKEN_PERSISTENCE_WARNED.store(false, Ordering::Release);
        return;
    };
    crate::log_persist!(
        error,
        "Could not store {} sign-in secret(s) in the keyring or the fallback token file: {}",
        failed_keys.len(),
        failed_keys.join(", ")
    );
    if !TOKEN_PERSISTENCE_WARNED.swap(true, Ordering::AcqRel) {
        super::push_ui_action(super::UiAction::ShowWarningToast {
            message: "Sign-in could not be saved".to_string(),
            detail: Some(
                "Echo could not store your Twitch tokens securely. You stay signed in until \
                 Echo closes, but you may need to sign in again next time."
                    .to_string(),
            ),
        });
    }
}

/// settings.json parsed (or was absent, or this process wrote it).
const PRIMARY_GOOD: u8 = 0;
/// settings.json failed to load and a quarantine copy of it exists.
const PRIMARY_PRESERVED: u8 = 1;
/// settings.json failed to load and could not be copied aside yet.
const PRIMARY_UNPRESERVED: u8 = 2;
/// Shared by every `SettingsManager`: the load and the save worker use
/// separate instances.
static SETTINGS_PRIMARY_STATE: AtomicU8 = AtomicU8::new(PRIMARY_GOOD);
/// Set once the user was told saves are being refused; cleared by a save.
static SETTINGS_SAVE_REFUSED_WARNED: AtomicBool = AtomicBool::new(false);
/// Set when startup found neither settings.json nor a backup of it.
static SETTINGS_FILE_MISSING_AT_LOAD: AtomicBool = AtomicBool::new(false);

pub struct SettingsManager {
    file_path: PathBuf,
}

impl SettingsManager {
    pub fn new() -> Self {
        let config_dir = crate::benchmark_profile::config_root_or_current_dir();
        if let Err(e) = fs::create_dir_all(&config_dir) {
            tracing::warn!(
                "Failed to create config directory '{}': {}",
                config_dir.display(),
                e
            );
        }

        let file_path = config_dir.join("settings.json");

        let legacy_path = crate::benchmark_profile::legacy_cwd_file("settings.json");
        if !file_path.exists()
            && !crate::utils::backup_generation_paths(&file_path)
                .iter()
                .any(|path| path.exists())
            && legacy_path.as_ref().is_some_and(|path| path.exists())
        {
            let legacy_path = legacy_path.expect("checked above");
            let migrated = fs::read_to_string(&legacy_path)
                .map_err(|e| e.to_string())
                .and_then(|json| {
                    strip_launch_settings_from_json(&json)
                        .ok_or_else(|| "not a settings JSON object".to_string())
                })
                .and_then(|json| fs::write(&file_path, json).map_err(|e| e.to_string()));
            if let Err(e) = migrated {
                tracing::warn!(
                    "Failed to migrate legacy settings from '{}' to '{}': {}",
                    legacy_path.display(),
                    file_path.display(),
                    e
                );
            } else {
                tracing::info!(
                    "Migrated legacy settings file to '{}' without its browser, player and \
                     Streamlink launch settings",
                    file_path.display()
                );
            }
        }

        Self { file_path }
    }
}

impl Default for SettingsManager {
    fn default() -> Self {
        Self::new()
    }
}

impl SettingsManager {
    pub fn load(&self) -> AppSettings {
        if matches!(self.file_path.try_exists(), Ok(false))
            && !crate::utils::backup_generation_paths(&self.file_path)
                .iter()
                .any(|path| path.exists())
        {
            SETTINGS_FILE_MISSING_AT_LOAD.store(true, Ordering::Release);
            let mut defaults = AppSettings::default();
            secrets::hydrate_tokens_from_keyring(&mut defaults);
            reconcile_active_account_credentials(&mut defaults);
            normalize_runtime_settings(&mut defaults);
            CACHED_EMOTE_IMAGE_SCALE
                .store(defaults.emote_image_scale.as_u8() as u64, Ordering::Release);
            return defaults;
        }

        let mut settings = match fs::read_to_string(&self.file_path) {
            Ok(content) => match serde_json::from_str(&content) {
                Ok(settings) => {
                    SETTINGS_PRIMARY_STATE.store(PRIMARY_GOOD, Ordering::Release);
                    settings
                }
                Err(e) => {
                    crate::log_persist!(
                        error,
                        "Settings file '{}' is unparseable ({}); recovering from backup",
                        self.file_path.display(),
                        crate::utils::json_error_summary(&e)
                    );
                    let preserved = self.quarantine_unloadable_primary();
                    self.recover_settings_from_backup(preserved)
                }
            },
            Err(e) => {
                crate::log_persist!(
                    error,
                    "Failed to read settings file '{}': {}; recovering from backup",
                    self.file_path.display(),
                    e
                );
                let preserved = if e.kind() == std::io::ErrorKind::NotFound {
                    SETTINGS_PRIMARY_STATE.store(PRIMARY_PRESERVED, Ordering::Release);
                    None
                } else {
                    self.quarantine_unloadable_primary()
                };
                self.recover_settings_from_backup(preserved)
            }
        };

        if secrets::has_plaintext_runtime_tokens(&settings) {
            report_token_persistence(secrets::persist_tokens_to_keyring(&settings));
        }
        secrets::hydrate_tokens_from_keyring(&mut settings);
        reconcile_active_account_credentials(&mut settings);
        normalize_runtime_settings(&mut settings);
        CACHED_EMOTE_IMAGE_SCALE
            .store(settings.emote_image_scale.as_u8() as u64, Ordering::Release);
        settings
    }

    /// Copy a settings.json that failed to load aside before anything can
    /// overwrite it, and record whether that worked so `save()` knows it must
    /// not copy the file over the backup.
    fn quarantine_unloadable_primary(&self) -> Option<PathBuf> {
        match crate::utils::quarantine_corrupt_file(&self.file_path) {
            Ok(path) => {
                SETTINGS_PRIMARY_STATE.store(PRIMARY_PRESERVED, Ordering::Release);
                crate::log_persist!(
                    warn,
                    "Preserved the unloadable settings file as '{}'",
                    path.display()
                );
                Some(path)
            }
            Err(e) => {
                SETTINGS_PRIMARY_STATE.store(PRIMARY_UNPRESERVED, Ordering::Release);
                crate::log_persist!(
                    error,
                    "Could not preserve the unloadable settings file '{}': {}",
                    self.file_path.display(),
                    e
                );
                None
            }
        }
    }

    fn recover_settings_from_backup(&self, preserved_primary: Option<PathBuf>) -> AppSettings {
        let mut preserved: Vec<_> = preserved_primary.into_iter().collect();
        let mut unpreserved = Vec::new();
        if SETTINGS_PRIMARY_STATE.load(Ordering::Acquire) == PRIMARY_UNPRESERVED {
            unpreserved.push(self.file_path.clone());
        }
        for backup_path in crate::utils::backup_generation_paths(&self.file_path) {
            let error = match fs::read_to_string(&backup_path) {
                Ok(content) => match serde_json::from_str(&content) {
                    Ok(settings) => {
                        super::reliability::note_storage_recovery(
                            "Settings",
                            &self.file_path,
                            Some(&backup_path),
                            &preserved,
                            &unpreserved,
                        );
                        return settings;
                    }
                    Err(error) => crate::utils::json_error_summary(&error),
                },
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => error.to_string(),
            };
            crate::log_persist!(
                warn,
                "Settings backup '{}' is unusable: {}",
                backup_path.display(),
                error
            );
            match crate::utils::quarantine_corrupt_file(&backup_path) {
                Ok(path) => preserved.push(path),
                Err(_) => unpreserved.push(backup_path),
            }
        }
        super::reliability::note_storage_recovery(
            "Settings",
            &self.file_path,
            None,
            &preserved,
            &unpreserved,
        );
        AppSettings::default()
    }

    pub fn save(&self, settings: &AppSettings) -> Result<(), String> {
        // Tokens never go to settings.json: when they cannot be stored, they
        // stay in memory for this session and the user is warned.
        report_token_persistence(secrets::persist_tokens_to_keyring(settings));
        let scrubbed = scrub_tokens_for_disk(settings);
        let content = serde_json::to_string_pretty(&scrubbed).map_err(|e| e.to_string())?;
        let temp_path = self.file_path.with_extension("json.tmp");

        let primary_state = SETTINGS_PRIMARY_STATE.load(Ordering::Acquire);
        if primary_state == PRIMARY_UNPRESERVED && self.file_path.exists() {
            match crate::utils::quarantine_corrupt_file(&self.file_path) {
                Ok(path) => {
                    SETTINGS_PRIMARY_STATE.store(PRIMARY_PRESERVED, Ordering::Release);
                    crate::log_persist!(
                        warn,
                        "Preserved the unloadable settings file as '{}'",
                        path.display()
                    );
                }
                Err(e) => {
                    if !SETTINGS_SAVE_REFUSED_WARNED.swap(true, Ordering::AcqRel) {
                        super::push_ui_action(super::UiAction::ShowWarningToast {
                            message: "Settings can't be saved".to_string(),
                            detail: Some(
                                "The settings file couldn't be backed up. Changes may be lost \
                                 at exit."
                                    .to_string(),
                            ),
                        });
                    }
                    return Err(format!(
                        "refusing to overwrite '{}': it failed to load and could not be preserved ({e})",
                        self.file_path.display()
                    ));
                }
            }
        }

        fs::write(&temp_path, content).map_err(|e| e.to_string())?;
        // Only a settings.json that loaded (or that this process wrote) may
        // become the backup; copying one that failed to parse would destroy
        // the last good copy.
        if primary_state == PRIMARY_GOOD && self.file_path.exists() {
            // A failed backup refresh must not cost the user this save.
            if let Err(error) = crate::utils::rotate_known_good_backups(&self.file_path, |bytes| {
                serde_json::from_slice::<AppSettings>(bytes).is_ok()
            }) {
                crate::log_persist!(warn, "Failed to refresh settings backups: {}", error);
            }
        }

        if let Err(error) = crate::utils::replace_file(&temp_path, &self.file_path) {
            let _ = fs::remove_file(&temp_path);
            return Err(error.to_string());
        }
        SETTINGS_PRIMARY_STATE.store(PRIMARY_GOOD, Ordering::Release);
        SETTINGS_SAVE_REFUSED_WARNED.store(false, Ordering::Release);
        Ok(())
    }

    pub fn create_import_backup(&self) -> Result<Option<PathBuf>, String> {
        if !self.file_path.exists() {
            return Ok(None);
        }

        let backup_dir = self
            .file_path
            .parent()
            .map(|path| path.join("settings-backups"))
            .unwrap_or_else(|| PathBuf::from("settings-backups"));
        fs::create_dir_all(&backup_dir).map_err(|e| e.to_string())?;

        let timestamp_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_millis())
            .unwrap_or(0);
        let backup_path = backup_dir.join(format!("settings-before-import-{timestamp_ms}.json"));
        fs::copy(&self.file_path, &backup_path).map_err(|e| e.to_string())?;
        Ok(Some(backup_path))
    }
}

fn publish_cached_settings_snapshot(settings: &AppSettings) {
    crate::emote_renderer::set_animation_enabled(settings.animate_emotes);
    CACHED_EMOTE_IMAGE_SCALE.store(settings.emote_image_scale.as_u8() as u64, Ordering::Release);
    CACHED_WINDOW_TRANSPARENT.store(settings.window_transparent, Ordering::Release);
    CACHED_MINIMIZE_TO_TRAY.store(settings.minimize_to_tray, Ordering::Release);
    CACHED_CLOSE_TO_TRAY.store(settings.close_to_tray, Ordering::Release);
    CACHED_ANONYMOUS_MODE.store(settings.anonymous_mode, Ordering::Release);
    CACHED_ENABLE_TWITCH_BLOCKED_USERS
        .store(settings.enable_twitch_blocked_users, Ordering::Release);
    CACHED_SHOW_BLOCKED_USERS_MESSAGES.store(
        settings.show_blocked_users_messages.as_u64(),
        Ordering::Release,
    );
    CACHED_EXCLUDE_USER_MESSAGES_FROM_FILTER.store(
        settings.exclude_user_messages_from_filter,
        Ordering::Release,
    );
    CACHED_CHAT_HOVER_PAUSE_MODE.store(settings.chat_hover_pause_mode as u8, Ordering::Release);
    CACHED_CHAT_PAUSE_MODIFIER.store(settings.chat_pause_modifier as u8, Ordering::Release);
    CACHED_USERNAME_RIGHT_CLICK_BEHAVIOR.store(
        settings.username_right_click_behavior as u8,
        Ordering::Release,
    );
    CACHED_USERNAME_RIGHT_CLICK_MODIFIER_BEHAVIOR.store(
        settings.username_right_click_modifier_behavior as u8,
        Ordering::Release,
    );
    CACHED_USERNAME_RIGHT_CLICK_MODIFIER.store(
        settings.username_right_click_modifier as u8,
        Ordering::Release,
    );
    CACHED_SHOW_REPLY_BUTTON.store(settings.show_reply_button, Ordering::Release);
    CACHED_SHOW_LAST_READ_INDICATOR.store(settings.show_last_read_indicator, Ordering::Release);
    CACHED_ALTERNATE_MESSAGE_BACKGROUNDS
        .store(settings.alternate_message_backgrounds, Ordering::Release);
    CACHED_SHOW_PAUSE_INDICATOR.store(settings.show_pause_indicator, Ordering::Release);
    CACHED_MOUSE_SCROLL_MULTIPLIER.store(
        clamp_mouse_scroll_multiplier(settings.mouse_scroll_multiplier).to_bits(),
        Ordering::Release,
    );
    CACHED_SHOW_LIVE_CHATTER_INDICATOR
        .store(settings.show_live_chatter_indicator, Ordering::Release);
    CACHED_SHOW_MESSAGE_TIMESTAMPS.store(settings.show_message_timestamps, Ordering::Release);
    CACHED_HIDE_MESSAGE_TIMESTAMPS_WHEN_LIVE.store(
        settings.hide_message_timestamps_when_live,
        Ordering::Release,
    );
    highlight_matcher::publish(&settings.highlight_rules);
    publish_compiled_nickname_rules(&settings.nicknames);
    publish_compiled_emote_display_rules(&settings.emote_display_rules);
    match settings.message_timestamp_pattern.as_ref() {
        Some(pattern) => publish_compiled_timestamp_formatter(pattern),
        None => publish_compiled_timestamp_formatter(&TimestampPattern::default()),
    }
}

pub static SETTINGS: LazyLock<Mutex<AppSettings>> = LazyLock::new(|| {
    let manager = SettingsManager::new();
    let loaded = manager.load();
    crate::emote_renderer::set_animation_enabled(loaded.animate_emotes);
    publish_cached_settings_snapshot(&loaded);
    Mutex::new(loaded)
});

#[allow(clippy::large_enum_variant)]
enum SettingsSaveRequest {
    Snapshot(AppSettings),
    /// A snapshot written exactly as given, never merged into a later one,
    /// whose caller waits for the write's result.
    Reported(AppSettings, std::sync::mpsc::Sender<Result<(), String>>),
    Flush(std::sync::mpsc::Sender<()>),
}

/// Global async save channel. Saves happen on a dedicated worker thread.
static SETTINGS_SAVE_TX: OnceLock<std::sync::mpsc::Sender<SettingsSaveRequest>> = OnceLock::new();

fn settings_save_tx() -> &'static std::sync::mpsc::Sender<SettingsSaveRequest> {
    SETTINGS_SAVE_TX.get_or_init(|| {
        let (tx, rx) = std::sync::mpsc::channel::<SettingsSaveRequest>();

        std::thread::spawn(move || {
            let manager = SettingsManager::new();

            let save = |settings: &AppSettings| {
                let result = manager.save(settings);
                if let Err(e) = &result {
                    crate::log_persist!(error, "Failed to save settings: {e}");
                }
                result
            };

            while let Ok(request) = rx.recv() {
                match request {
                    SettingsSaveRequest::Snapshot(mut latest) => {
                        let mut flush_acks = Vec::new();
                        let mut reported = None;
                        while let Ok(next) = rx.try_recv() {
                            match next {
                                SettingsSaveRequest::Snapshot(next_settings) => {
                                    latest = next_settings;
                                }
                                SettingsSaveRequest::Reported(settings, report) => {
                                    // Keep queue order: the older snapshot is
                                    // written first, then this one as given.
                                    reported = Some((settings, report));
                                    break;
                                }
                                SettingsSaveRequest::Flush(ack) => {
                                    flush_acks.push(ack);
                                }
                            }
                        }

                        let _ = save(&latest);
                        if let Some((settings, report)) = reported {
                            let _ = report.send(save(&settings));
                        }

                        for ack in flush_acks {
                            let _ = ack.send(());
                        }
                    }
                    SettingsSaveRequest::Reported(settings, report) => {
                        let _ = report.send(save(&settings));
                    }
                    SettingsSaveRequest::Flush(ack) => {
                        let _ = ack.send(());
                    }
                }
            }
        });

        tx
    })
}

pub fn save_settings() {
    let snapshot = SETTINGS.lock_or_recover().clone();
    if settings_save_tx()
        .send(SettingsSaveRequest::Snapshot(snapshot))
        .is_err()
    {
        tracing::error!("Settings save worker unavailable");
    }
}

/// Wait until the async settings worker has processed all queued saves.
/// This is used during shutdown to avoid losing the latest in-memory state.
pub fn flush_settings_saves(timeout: std::time::Duration) {
    let (ack_tx, ack_rx) = std::sync::mpsc::channel::<()>();
    if settings_save_tx()
        .send(SettingsSaveRequest::Flush(ack_tx))
        .is_err()
    {
        tracing::error!("Settings save worker unavailable during flush");
        return;
    }
    if ack_rx.recv_timeout(timeout).is_err() {
        tracing::warn!("Timed out waiting for settings save flush");
    }
}

/// Write the current settings through the save worker and wait for the result.
pub fn save_settings_and_wait(timeout: std::time::Duration) -> Result<(), String> {
    let snapshot = SETTINGS.lock_or_recover().clone();
    save_snapshot_and_wait(snapshot, timeout)
}

/// Write `snapshot` (not the live settings) through the save worker and wait
/// for the result. An importer writes its staged settings this way before
/// anything in memory changes.
pub fn save_snapshot_and_wait(
    snapshot: AppSettings,
    timeout: std::time::Duration,
) -> Result<(), String> {
    let (report_tx, report_rx) = std::sync::mpsc::channel();
    settings_save_tx()
        .send(SettingsSaveRequest::Reported(snapshot, report_tx))
        .map_err(|_| "the settings save worker is unavailable".to_string())?;
    report_rx
        .recv_timeout(timeout)
        .map_err(|_| "timed out waiting for settings to save".to_string())?
}

/// Whether this run started without any Echo settings file (a first run).
pub fn settings_file_was_missing_at_startup() -> bool {
    LazyLock::force(&SETTINGS);
    SETTINGS_FILE_MISSING_AT_LOAD.load(Ordering::Acquire)
}

/// Path to Echo's active settings file.
pub fn settings_file_path() -> PathBuf {
    SettingsManager::new().file_path
}

/// Directory containing Echo's active settings file.
pub fn settings_dir_path() -> PathBuf {
    settings_file_path()
        .parent()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

/// Directory containing timestamped settings import backups.
pub fn settings_backups_dir_path() -> PathBuf {
    settings_dir_path().join("settings-backups")
}

/// Export the current settings snapshot as disk-safe JSON.
///
/// Runtime OAuth tokens are omitted, matching normal settings persistence.
/// Uploader header values and proxy credentials are also removed, since an
/// export is meant to be shared.
pub fn export_settings_to_json() -> Result<String, String> {
    let snapshot = SETTINGS.lock_or_recover().clone();
    let scrubbed = scrub_secrets_for_export(&snapshot);
    serde_json::to_string_pretty(&scrubbed).map_err(|e| e.to_string())
}

#[derive(Clone, Debug)]
pub struct SettingsImportResult {
    pub backup_path: Option<PathBuf>,
    /// Sensitive settings (launch, file paths, uploader, proxy) present in the
    /// import that were not applied.
    pub skipped_launch_settings: Vec<&'static str>,
}

fn replace_settings_snapshot(
    mut replacement: AppSettings,
    keep_sensitive_settings: bool,
) -> Result<SettingsImportResult, String> {
    let mut skipped_launch_settings = if keep_sensitive_settings {
        let current = SETTINGS.lock_or_recover().clone();
        keep_current_sensitive_settings(&mut replacement, &current)
    } else {
        Vec::new()
    };
    normalize_runtime_settings(&mut replacement);
    let blocked_users_enabled = replacement.enable_twitch_blocked_users;
    let backup_path = SettingsManager::new().create_import_backup()?;

    let visibility_commit = crate::backend::blocked_users::visibility_commit_guard();
    {
        let mut settings = SETTINGS.lock_or_recover();
        if keep_sensitive_settings {
            // Re-apply against the live settings under the lock, so a path or
            // launch setting changed since the first pass is not overwritten.
            for label in keep_current_sensitive_settings(&mut replacement, &settings) {
                if !skipped_launch_settings.contains(&label) {
                    skipped_launch_settings.push(label);
                }
            }
        }
        preserve_live_auth_state(&mut replacement, &settings);
        reconcile_active_account_credentials(&mut replacement);
        publish_cached_settings_snapshot(&replacement);
        update_username_cache(replacement.username.as_deref());
        *settings = replacement;
    }
    let (changed_channels, visibility_effects) = if blocked_users_enabled {
        (
            crate::backend::irc::apply_blocked_user_filter_to_buffers_visibility_commit_held(),
            None,
        )
    } else {
        (
            Default::default(),
            Some(
                crate::backend::irc::finalize_pending_blocked_visibility_commit_held(
                    crate::backend::blocked_users::current_auth_epoch_visibility_commit_held(),
                ),
            ),
        )
    };
    drop(visibility_commit);
    crate::backend::irc::dispatch_blocked_channel_refreshes(changed_channels);
    if let Some(effects) = visibility_effects {
        crate::backend::irc::dispatch_blocked_visibility_effects(effects);
    }

    save_settings();
    bump_layout_settings_version();
    broadcast_imported_settings_events();
    Ok(SettingsImportResult {
        backup_path,
        skipped_launch_settings,
    })
}

/// Import settings from a JSON snapshot.
///
/// The live Twitch account collection, credentials, and anonymous-mode state are
/// runtime identity, not importable preferences. They are preserved atomically
/// at publication because exported settings intentionally omit credentials.
/// Sensitive settings are also kept (see [`keep_current_sensitive_settings`]),
/// so a shared file cannot choose what Echo executes, which files it touches,
/// or where it sends data; the result lists the skipped ones.
pub fn import_settings_from_json(json: &str) -> Result<SettingsImportResult, String> {
    let imported: AppSettings =
        serde_json::from_str(json).map_err(|e| format!("Failed to parse settings JSON: {e}"))?;
    replace_settings_snapshot(imported, true)
}

/// Reset settings to defaults while preserving the current Twitch accounts.
pub fn reset_settings_to_defaults() -> Result<SettingsImportResult, String> {
    replace_settings_snapshot(AppSettings::default(), false)
}

fn preserve_account_state(defaults: &mut AppSettings, current: &AppSettings) {
    defaults.username = current.username.clone();
    defaults.token = current.token.clone();
    defaults.refresh_token = current.refresh_token.clone();
    defaults.client_id = current.client_id.clone();
    defaults.accounts = current.accounts.clone();
    defaults.token_expires_at = current.token_expires_at;
}

fn preserve_live_auth_state(settings: &mut AppSettings, current: &AppSettings) {
    preserve_account_state(settings, current);
    settings.anonymous_mode = current.anonymous_mode;
}

fn broadcast_imported_settings_events() {
    super::http::mark_proxy_settings_changed();
    crate::backend::embeds::cache::EMBED_CACHE.clear();
    request_top_bar_state_refresh();
    for event in [
        SettingsEvent::ThemeChanged,
        SettingsEvent::FontScaleChanged,
        SettingsEvent::EmoteScaleChanged,
        SettingsEvent::BadgeScaleChanged,
        SettingsEvent::AccentChanged,
        SettingsEvent::EmoteSpacingChanged,
        SettingsEvent::ChatDensityChanged,
        SettingsEvent::HighlightColorChanged,
        SettingsEvent::EmbedSettingsChanged,
        SettingsEvent::TabThemeChanged,
        SettingsEvent::TabOverflowModeChanged,
        SettingsEvent::TabPlacementChanged,
        SettingsEvent::TitlebarDensityChanged,
        SettingsEvent::BadgeDisplayModeChanged,
        SettingsEvent::AnimationPerformanceChanged,
        SettingsEvent::CosmeticSettingsChanged,
        SettingsEvent::EmoteDisplayRulesChanged,
        SettingsEvent::StreamerModeChanged,
        SettingsEvent::WindowAppearanceChanged,
        SettingsEvent::OverlaySettingsChanged,
        SettingsEvent::ChatPauseChanged,
        SettingsEvent::TimestampChanged,
        SettingsEvent::ProxyChanged,
        SettingsEvent::FiltersChanged,
    ] {
        broadcast_settings_event(event);
    }
}

/// Save settings to disk, but debounce rapid consecutive updates.
///
/// Sliders (font scale / emote scale) can emit dozens of updates per second during drag.
/// Writing `settings.json` for every tick is unnecessary I/O and can contribute to UI jank.
fn save_settings_debounced() {
    use std::sync::mpsc;
    use std::time::Duration;

    static SAVE_TX: OnceLock<mpsc::Sender<()>> = OnceLock::new();

    let tx = SAVE_TX.get_or_init(|| {
        let (tx, rx) = mpsc::channel::<()>();

        std::thread::spawn(move || {
            loop {
                if rx.recv().is_err() {
                    break;
                }

                while rx.recv_timeout(Duration::from_millis(250)).is_ok() {}

                save_settings();
            }
        });

        tx
    });

    let _ = tx.send(());
}

pub fn get_token() -> Option<String> {
    SETTINGS
        .lock_or_recover()
        .token
        .as_deref()
        .map(normalize_runtime_oauth_token)
        .filter(|token| !token.is_empty())
}

pub fn get_username() -> Option<String> {
    SETTINGS.lock_or_recover().username.clone()
}

fn normalize_runtime_oauth_token(token: &str) -> String {
    let trimmed = token.trim();
    if let Some(rest) = trimmed.strip_prefix("oauth:") {
        return rest.trim().to_string();
    }
    if trimmed
        .get(..7)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("Bearer "))
    {
        return trimmed[7..].trim().to_string();
    }
    trimmed.to_string()
}

/// PERF: Get cached lowercase username for mention detection (lock-free clone)
/// Returns empty string if not logged in
pub fn get_username_lowercase() -> String {
    CACHED_USERNAME_LOWERCASE.lock_or_recover().clone()
}

pub fn is_signed_in_username(login: &str) -> bool {
    let cached = CACHED_USERNAME_LOWERCASE.lock_or_recover();
    !cached.is_empty() && cached.eq_ignore_ascii_case(login)
}

/// Update the cached lowercase username (call when username changes)
fn update_username_cache(username: Option<&str>) {
    let mut cache = CACHED_USERNAME_LOWERCASE.lock_or_recover();
    *cache = username.map(|u| u.to_lowercase()).unwrap_or_default();
}

/// Maximum number of recent channels to track
const MAX_RECENT_CHANNELS: usize = 5;

/// Track a channel visit for predictive emote preloading.
/// Adds channel to front of recent list, dedupes, and caps at MAX_RECENT_CHANNELS.
pub fn track_channel_visit(channel: &str) {
    let channel_lower = channel.to_lowercase();
    let mut settings = SETTINGS.lock_or_recover();
    settings
        .recent_channels
        .retain(|c| c.to_lowercase() != channel_lower);
    settings.recent_channels.insert(0, channel_lower);
    settings.recent_channels.truncate(MAX_RECENT_CHANNELS);
}

/// Get recently visited channels for predictive preloading.
/// Returns up to MAX_RECENT_CHANNELS in most-recent-first order.
pub fn get_recent_channels() -> Vec<String> {
    SETTINGS.lock_or_recover().recent_channels.clone()
}

pub fn get_theme() -> Option<String> {
    SETTINGS.lock_or_recover().theme.clone()
}

pub fn get_resolved_system_theme() -> ResolvedSystemTheme {
    SETTINGS.lock_or_recover().resolved_system_theme
}

pub fn get_resolved_system_theme_prefers_light() -> Option<bool> {
    get_resolved_system_theme().prefers_light()
}

pub fn get_system_theme_status_message() -> String {
    let resolved = get_resolved_system_theme();

    #[cfg(not(target_os = "windows"))]
    {
        let probe = detect_resolved_system_theme_probe();
        if probe.theme == resolved || resolved == ResolvedSystemTheme::Unknown {
            return probe.status_message();
        }
    }

    match resolved {
        ResolvedSystemTheme::Light => "Following the OS light theme".to_string(),
        ResolvedSystemTheme::Dark => "Following the OS dark theme".to_string(),
        ResolvedSystemTheme::Unknown => {
            #[cfg(target_os = "windows")]
            {
                "OS theme detection unavailable from Windows personalization; using the dark fallback"
                    .to_string()
            }
            #[cfg(not(target_os = "windows"))]
            {
                "OS theme detection unavailable; using the dark fallback".to_string()
            }
        }
    }
}

pub fn is_system_theme_active() -> bool {
    theme_uses_system(SETTINGS.lock_or_recover().theme.as_deref())
}

pub fn get_emote_scale() -> f32 {
    SETTINGS.lock_or_recover().emote_scale
}

pub fn get_emote_image_scale() -> EmoteImageScale {
    EmoteImageScale::from_u8(CACHED_EMOTE_IMAGE_SCALE.load(Ordering::Acquire) as u8)
}

/// Current window DPI scale (defaults to 1.0 until the first frame syncs it).
pub fn get_window_scale_factor() -> f32 {
    f32::from_bits(CACHED_WINDOW_SCALE_FACTOR.load(Ordering::Acquire) as u32)
}

/// Record the main window's DPI scale. Each chat canvas re-lays out for its own
/// window's scale, so this never bumps the layout version; a material change
/// only broadcasts `WindowScaleChanged` so tier-picking views refresh.
pub fn set_window_scale_factor(scale: f32) {
    if !scale.is_finite() || scale <= 0.0 {
        return;
    }
    let previous = CACHED_WINDOW_SCALE_FACTOR.swap(scale.to_bits() as u64, Ordering::AcqRel);
    if (scale - f32::from_bits(previous as u32)).abs() > 0.02 {
        broadcast_settings_event(SettingsEvent::WindowScaleChanged);
    }
}

pub fn get_primary_accent_color() -> String {
    SETTINGS.lock_or_recover().primary_accent_color.clone()
}

pub fn get_switch_on_style() -> SwitchOnStyle {
    SETTINGS.lock_or_recover().switch_on_style
}

pub fn get_primary_text_color() -> Option<String> {
    SETTINGS.lock_or_recover().primary_text_color.clone()
}

pub fn get_embed_display_mode() -> EmbedDisplayMode {
    SETTINGS.lock_or_recover().embed_display_mode.clone()
}

/// Whether this classified resource is eligible for previews under the
/// persisted provider/content policy. The global display mode is intentionally
/// evaluated by the scheduler, not hidden in this predicate.
pub fn is_embed_classification_enabled(
    provider: crate::backend::link_types::Provider,
    content_kind: crate::backend::link_types::ContentKind,
) -> bool {
    use crate::backend::link_types::{ContentKind, Provider};
    let settings = SETTINGS.lock_or_recover();
    let provider_enabled = match provider {
        Provider::Direct => settings.embed_providers.images,
        Provider::SevenTv => settings.embed_providers.seventv,
        Provider::Spotify => settings.embed_providers.spotify,
        Provider::Steam => settings.embed_providers.steam,
        Provider::Twitch => settings.embed_providers.twitch,
        Provider::YouTube => settings.embed_providers.youtube,
        Provider::Twitter => settings.embed_providers.twitter,
        Provider::Reddit => settings.embed_providers.reddit,
        Provider::Generic => settings.embed_providers.generic,
    };
    let content_enabled = match content_kind {
        ContentKind::Image => settings.embed_content.images,
        ContentKind::Audio => settings.embed_content.generic,
        ContentKind::Clip => settings.embed_content.clips,
        ContentKind::Video => settings.embed_content.videos,
        ContentKind::Channel => settings.embed_content.generic,
        ContentKind::Emote | ContentKind::Profile => settings.embed_content.generic,
        ContentKind::Post => settings.embed_content.posts,
        ContentKind::Generic => settings.embed_content.generic,
    };
    provider_enabled && content_enabled
}

pub fn get_embed_seventv_enabled() -> bool {
    SETTINGS.lock_or_recover().embed_providers.seventv
}

pub fn get_embed_spotify_enabled() -> bool {
    SETTINGS.lock_or_recover().embed_providers.spotify
}

pub fn get_embed_steam_enabled() -> bool {
    SETTINGS.lock_or_recover().embed_providers.steam
}

fn set_embed_provider_enabled(
    enabled: bool,
    select: impl FnOnce(&mut EmbedProviderSettings) -> &mut bool,
) {
    {
        let mut settings = SETTINGS.lock_or_recover();
        let provider_enabled = select(&mut settings.embed_providers);
        if *provider_enabled == enabled {
            return;
        }
        *provider_enabled = enabled;
    }
    crate::backend::embeds::cache::EMBED_CACHE.clear();
    crate::backend::embeds::cache::RESOURCE_FLIGHTS.invalidate_policy();
    save_settings();
    crate::components::chat::canvas_renderer::global_layout_cache::clear_embed_layouts();
    crate::state::VersionRegistry::bump_embed_metadata_version();
    broadcast_settings_event(SettingsEvent::EmbedSettingsChanged);
}

pub fn set_embed_spotify_enabled(enabled: bool) {
    set_embed_provider_enabled(enabled, |providers| &mut providers.spotify);
}

pub fn set_embed_steam_enabled(enabled: bool) {
    set_embed_provider_enabled(enabled, |providers| &mut providers.steam);
}

pub fn set_embed_seventv_enabled(enabled: bool) {
    set_embed_provider_enabled(enabled, |providers| &mut providers.seventv);
}

pub fn get_embed_remote_fetch_enabled() -> bool {
    SETTINGS.lock_or_recover().embed_remote_fetch_enabled
}

pub fn get_embed_thumbnail_mode() -> EmbedThumbnailMode {
    SETTINGS.lock_or_recover().embed_thumbnail_mode
}

pub fn get_tab_overflow_mode() -> TabOverflowMode {
    SETTINGS.lock_or_recover().tab_overflow_mode.clone()
}

pub fn get_tab_placement() -> TabPlacement {
    SETTINGS.lock_or_recover().tab_placement.clone()
}

pub fn get_titlebar_density() -> TitlebarDensity {
    SETTINGS.lock_or_recover().titlebar_density
}

pub fn titlebar_height_px() -> f32 {
    get_titlebar_density().height_px()
}

pub fn get_remove_emote_spacing() -> bool {
    SETTINGS.lock_or_recover().remove_emote_spacing
}

pub fn get_zero_width_enabled() -> bool {
    SETTINGS.lock_or_recover().zero_width_enabled
}

pub fn get_chat_density() -> ChatDensity {
    SETTINGS.lock_or_recover().chat_density
}

pub fn set_chat_density(density: ChatDensity) {
    {
        let mut settings = SETTINGS.lock_or_recover();
        if settings.chat_density == density {
            return;
        }
        settings.chat_density = density;
    }
    save_settings_debounced();
    bump_layout_settings_version();
    broadcast_settings_event(SettingsEvent::ChatDensityChanged);
}

pub fn get_username_display_mode() -> UsernameDisplayMode {
    SETTINGS.lock_or_recover().username_display_mode
}

pub fn set_username_display_mode(mode: UsernameDisplayMode) {
    {
        let mut settings = SETTINGS.lock_or_recover();
        if settings.username_display_mode == mode {
            return;
        }
        settings.username_display_mode = mode;
    }
    save_settings();
    bump_layout_settings_version();
    broadcast_settings_event(SettingsEvent::CosmeticSettingsChanged);
}

pub fn get_colorize_users_without_color() -> bool {
    SETTINGS.lock_or_recover().colorize_users_without_color
}

pub fn set_colorize_users_without_color(enabled: bool) {
    {
        let mut settings = SETTINGS.lock_or_recover();
        if settings.colorize_users_without_color == enabled {
            return;
        }
        settings.colorize_users_without_color = enabled;
    }
    save_settings();
    bump_layout_settings_version();
    broadcast_settings_event(SettingsEvent::CosmeticSettingsChanged);
}

pub fn get_show_live_chatter_indicator() -> bool {
    CACHED_SHOW_LIVE_CHATTER_INDICATOR.load(Ordering::Acquire)
}

pub fn set_show_live_chatter_indicator(show: bool) {
    {
        let mut settings = SETTINGS.lock_or_recover();
        if settings.show_live_chatter_indicator == show {
            return;
        }
        settings.show_live_chatter_indicator = show;
    }
    CACHED_SHOW_LIVE_CHATTER_INDICATOR.store(show, Ordering::Release);
    save_settings();
    bump_layout_settings_version();
    broadcast_settings_event(SettingsEvent::CosmeticSettingsChanged);
}

/// Format Twitch's canonical login and localized display-name tag for presentation.
/// Case-only display-name differences retain Twitch's capitalization in every mode.
pub fn format_username_for_display(login: &str, display_name: &str) -> String {
    let display_name = display_name.trim();
    if display_name.is_empty() {
        return login.to_string();
    }
    if display_name.eq_ignore_ascii_case(login) {
        return display_name.to_string();
    }

    match get_username_display_mode() {
        UsernameDisplayMode::Username => login.to_string(),
        UsernameDisplayMode::LocalizedName => display_name.to_string(),
        UsernameDisplayMode::UsernameAndLocalizedName => {
            format!("{login}({display_name})")
        }
    }
}

pub fn set_theme(theme: String) {
    let resolved_system_theme =
        theme_uses_system(Some(theme.as_str())).then(detect_resolved_system_theme);
    {
        let mut settings = SETTINGS.lock_or_recover();
        let unchanged = settings.theme.as_deref() == Some(theme.as_str())
            && resolved_system_theme
                .is_none_or(|resolved| settings.resolved_system_theme == resolved);
        if unchanged {
            return;
        }
        settings.theme = Some(theme);
        if let Some(resolved_system_theme) = resolved_system_theme {
            settings.resolved_system_theme = resolved_system_theme;
        }
    }
    save_settings();
    broadcast_theme_changed();
}

pub fn set_font_scale(scale: f32) {
    let scale = scale.clamp(0.5, 3.0);
    {
        let mut settings = SETTINGS.lock_or_recover();
        if (settings.font_scale - scale).abs() < 0.0001 {
            return;
        }
        settings.font_scale = scale;
    }
    save_settings_debounced();
    bump_layout_settings_version();
    broadcast_settings_event(SettingsEvent::FontScaleChanged);
}

pub fn get_font_scale() -> f32 {
    SETTINGS.lock_or_recover().font_scale
}

pub fn set_emote_scale(scale: f32) {
    let scale = scale.clamp(0.25, 4.0);
    {
        let mut settings = SETTINGS.lock_or_recover();
        if (settings.emote_scale - scale).abs() < 0.0001 {
            return;
        }
        settings.emote_scale = scale;
    }
    save_settings_debounced();
    bump_layout_settings_version();
    broadcast_settings_event(SettingsEvent::EmoteScaleChanged);
}

pub fn set_emote_image_scale(scale: EmoteImageScale) {
    {
        let mut settings = SETTINGS.lock_or_recover();
        if settings.emote_image_scale == scale {
            return;
        }
        settings.emote_image_scale = scale;
        CACHED_EMOTE_IMAGE_SCALE.store(scale.as_u8() as u64, Ordering::Release);
    }
    save_settings_debounced();
    bump_layout_settings_version();
    broadcast_settings_event(SettingsEvent::EmoteScaleChanged);
}

pub fn get_badge_scale() -> f32 {
    SETTINGS.lock_or_recover().badge_scale
}

pub fn set_badge_scale(scale: f32) {
    let clamped = scale.clamp(0.5, 3.0);
    {
        let mut settings = SETTINGS.lock_or_recover();
        if (settings.badge_scale - clamped).abs() < f32::EPSILON {
            return;
        }
        settings.badge_scale = clamped;
    }
    save_settings_debounced();
    bump_layout_settings_version();
    broadcast_settings_event(SettingsEvent::BadgeScaleChanged);
}

pub fn set_primary_accent_color(color: String) {
    SETTINGS.lock_or_recover().primary_accent_color = color;
    save_settings_debounced();
    broadcast_settings_event(SettingsEvent::AccentChanged);
}

/// Persist the enabled-switch track style and invalidate the theme-dependent
/// switch palette immediately in every settings host.
pub fn set_switch_on_style(style: SwitchOnStyle) {
    {
        let mut settings = SETTINGS.lock_or_recover();
        if settings.switch_on_style == style {
            return;
        }
        settings.switch_on_style = style;
    }
    save_settings_debounced();
    broadcast_settings_event(SettingsEvent::ThemeChanged);
}

pub fn set_primary_text_color(color: Option<String>) {
    let color = color.and_then(|color| {
        let color = color.trim().to_string();
        hex_to_rgb(&color).map(|_| color)
    });
    {
        let mut settings = SETTINGS.lock_or_recover();
        if settings.primary_text_color == color {
            return;
        }
        settings.primary_text_color = color;
    }
    save_settings_debounced();
    broadcast_theme_changed();
}

pub fn get_custom_theme() -> CustomTheme {
    SETTINGS.lock_or_recover().custom_theme.clone()
}

/// Get the generated palette from the custom theme
pub fn get_custom_palette() -> GeneratedPalette {
    get_custom_theme().generate_palette()
}

pub fn get_custom_theme_file_path() -> String {
    SETTINGS.lock_or_recover().custom_theme_file_path.clone()
}

pub fn get_custom_theme_file_auto_reload() -> bool {
    SETTINGS.lock_or_recover().custom_theme_file_auto_reload
}

pub fn get_custom_theme_file_palette() -> Option<GeneratedPalette> {
    SETTINGS.lock_or_recover().custom_theme_file_palette.clone()
}

pub fn get_custom_theme_file_accent() -> Option<String> {
    SETTINGS.lock_or_recover().custom_theme_file_accent.clone()
}

pub fn load_custom_theme_file(path: String) -> Result<String, String> {
    let path = std::path::PathBuf::from(path.trim().trim_matches('"'));
    if path.as_os_str().is_empty() {
        return Err("Theme file path is empty".to_string());
    }
    if is_remote_or_device_path(&path.to_string_lossy()) {
        return Err(
            "Theme file must be on this computer, not a network or device path".to_string(),
        );
    }

    let parsed = crate::backend::theme_files::parse_theme_file(&path)?;
    let loaded_mtime = theme_file_mtime_ms(&path);
    let name = parsed
        .name
        .clone()
        .unwrap_or_else(|| theme_file_display_name(&path));

    {
        let mut settings = SETTINGS.lock_or_recover();
        settings.custom_theme_file_path = path.to_string_lossy().to_string();
        settings.custom_theme_file_accent = parsed.accent.clone();
        settings.custom_theme_file_palette = Some(parsed.into());
        settings.custom_theme_file_loaded_mtime_ms = loaded_mtime;
        settings.theme = Some("Theme File".to_string());
        if let Some(accent) = settings.custom_theme_file_accent.clone() {
            settings.primary_accent_color = accent;
        }
    }

    save_settings();
    broadcast_theme_changed();
    Ok(name)
}

pub fn set_custom_theme_file_auto_reload(enabled: bool) {
    {
        let mut settings = SETTINGS.lock_or_recover();
        if settings.custom_theme_file_auto_reload == enabled {
            return;
        }
        settings.custom_theme_file_auto_reload = enabled;
    }
    save_settings();
    broadcast_settings_event(SettingsEvent::ThemeChanged);
}

pub fn refresh_system_theme_resolution(force_broadcast: bool) -> bool {
    let detected = detect_resolved_system_theme();
    let changed = {
        let mut settings = SETTINGS.lock_or_recover();
        if settings.resolved_system_theme == detected {
            false
        } else {
            settings.resolved_system_theme = detected;
            true
        }
    };

    if changed {
        save_settings_debounced();
        broadcast_theme_changed();
    } else if force_broadcast {
        broadcast_settings_event(SettingsEvent::ThemeChanged);
    }

    changed
}

pub fn refresh_active_theme() -> Result<String, String> {
    let active_theme = get_theme().unwrap_or_else(|| "Dark".to_string());
    if active_theme.eq_ignore_ascii_case("theme file") {
        let path = get_custom_theme_file_path();
        if path.trim().is_empty() {
            return Err("No theme file is currently loaded".to_string());
        }
        let name = load_custom_theme_file(path)?;
        return Ok(format!("Reloaded theme file '{name}'"));
    }

    if active_theme.eq_ignore_ascii_case("system") {
        let changed = refresh_system_theme_resolution(true);
        let resolved = get_resolved_system_theme().label();
        if changed {
            Ok(format!("Resolved the current OS theme as {resolved}"))
        } else {
            Ok(format!("System theme unchanged ({resolved})"))
        }
    } else {
        broadcast_theme_changed();
        Ok(format!("Refreshed the {active_theme} theme"))
    }
}

pub fn reload_custom_theme_file_if_changed() -> Result<bool, String> {
    let (enabled, path, loaded_mtime) = {
        let settings = SETTINGS.lock_or_recover();
        (
            settings.custom_theme_file_auto_reload,
            settings.custom_theme_file_path.clone(),
            settings.custom_theme_file_loaded_mtime_ms,
        )
    };

    if !enabled || path.trim().is_empty() {
        return Ok(false);
    }

    let path_buf = std::path::PathBuf::from(path);
    let current_mtime = theme_file_mtime_ms(&path_buf);
    if current_mtime.is_some() && current_mtime == loaded_mtime {
        return Ok(false);
    }

    load_custom_theme_file(path_buf.to_string_lossy().to_string())?;
    Ok(true)
}

pub fn export_current_theme_file_json() -> Result<String, String> {
    let palette = {
        let settings = SETTINGS.lock_or_recover();
        let active_theme = settings.theme.as_deref().unwrap_or("Custom");
        let (name, generated) = if active_theme.eq_ignore_ascii_case("theme file") {
            let generated = settings
                .custom_theme_file_palette
                .clone()
                .ok_or_else(|| "No theme file palette is loaded".to_string())?;
            let name = if settings.custom_theme_file_path.trim().is_empty() {
                "Echo theme file".to_string()
            } else {
                theme_file_display_name(std::path::Path::new(&settings.custom_theme_file_path))
            };
            (name, generated)
        } else if active_theme.eq_ignore_ascii_case("custom") {
            (
                "Echo custom theme".to_string(),
                settings.custom_theme.generate_palette(),
            )
        } else {
            return Err("Theme export is available for Custom and Theme File modes".to_string());
        };

        crate::backend::theme_files::ThemeFilePalette {
            name: Some(name),
            accent: Some(settings.primary_accent_color.clone()),
            bg_void: generated.bg_void,
            bg_surface: generated.bg_surface,
            bg_plate: generated.bg_plate,
            bg_elevated: generated.bg_elevated,
            bg_hover: generated.bg_hover,
            text_primary: generated.text_primary,
            text_secondary: generated.text_secondary,
            text_muted: generated.text_muted,
            border: generated.border,
            border_strong: generated.border_strong,
        }
    };

    crate::backend::theme_files::export_echo_theme_json(&palette)
}

pub fn export_current_theme_file(path: String) -> Result<String, String> {
    let path = std::path::PathBuf::from(path.trim().trim_matches('"'));
    if path.as_os_str().is_empty() {
        return Err("Export path is empty".to_string());
    }

    let exported = export_current_theme_file_json()?;
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent).map_err(|e| {
            format!(
                "Failed to create theme export directory '{}': {e}",
                parent.display()
            )
        })?;
    }
    std::fs::write(&path, exported)
        .map_err(|e| format!("Failed to write theme file '{}': {e}", path.display()))?;
    Ok(path.to_string_lossy().to_string())
}

fn theme_file_display_name(path: &std::path::Path) -> String {
    path.file_stem()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or("Custom theme file")
        .to_string()
}

fn theme_file_mtime_ms(path: &std::path::Path) -> Option<i64> {
    let modified = std::fs::metadata(path).ok()?.modified().ok()?;
    let duration = modified
        .duration_since(std::time::SystemTime::UNIX_EPOCH)
        .ok()?;
    Some(duration.as_millis().min(i64::MAX as u128) as i64)
}

pub fn set_custom_theme_base_color(color: String) {
    {
        let mut settings = SETTINGS.lock_or_recover();
        if settings.custom_theme.base_color == color {
            return;
        }
        settings.custom_theme.base_color = color;
    }
    save_settings_debounced();
    if get_theme().as_deref() == Some("Custom") {
        broadcast_theme_changed();
    }
}

pub fn set_custom_theme_light_mode(is_light: bool) {
    {
        let mut settings = SETTINGS.lock_or_recover();
        if settings.custom_theme.is_light == is_light {
            return;
        }
        settings.custom_theme.is_light = is_light;
    }
    save_settings();
    if get_theme().as_deref() == Some("Custom") {
        broadcast_theme_changed();
    }
}

pub fn set_custom_theme_saturation(saturation: f32) {
    let saturation = saturation.clamp(0.0, 1.0);
    {
        let mut settings = SETTINGS.lock_or_recover();
        if settings.custom_theme.saturation == saturation {
            return;
        }
        settings.custom_theme.saturation = saturation;
    }
    save_settings();
    if get_theme().as_deref() == Some("Custom") {
        broadcast_theme_changed();
    }
}

/// Get the current theme mode
pub fn get_theme_mode() -> ThemeMode {
    SETTINGS.lock_or_recover().theme_mode
}

/// Set the theme mode
pub fn set_theme_mode(mode: ThemeMode) {
    {
        let mut settings = SETTINGS.lock_or_recover();
        if settings.theme_mode == mode {
            return;
        }
        settings.theme_mode = mode;
    }
    save_settings();
    broadcast_theme_changed();
}

pub fn set_badge_display_mode(mode: BadgeDisplayMode) {
    SETTINGS.lock_or_recover().badge_display_mode = mode;
    save_settings();
    broadcast_settings_event(SettingsEvent::BadgeDisplayModeChanged);
}

pub fn set_embed_display_mode(mode: EmbedDisplayMode) {
    {
        let mut settings = SETTINGS.lock_or_recover();
        if settings.embed_display_mode == mode {
            return;
        }
        settings.embed_display_mode = mode;
    }
    crate::backend::embeds::cache::RESOURCE_FLIGHTS.invalidate_policy();
    save_settings();
    crate::components::chat::canvas_renderer::global_layout_cache::clear_embed_layouts();
    crate::state::VersionRegistry::bump_embed_metadata_version();
    broadcast_settings_event(SettingsEvent::EmbedSettingsChanged);
}

pub fn set_embed_remote_fetch_enabled(enabled: bool) {
    {
        let mut settings = SETTINGS.lock_or_recover();
        if settings.embed_remote_fetch_enabled == enabled {
            return;
        }
        settings.embed_remote_fetch_enabled = enabled;
    }
    crate::backend::embeds::cache::EMBED_CACHE.clear();
    crate::backend::embeds::cache::RESOURCE_FLIGHTS.invalidate_policy();
    save_settings();
    crate::components::chat::canvas_renderer::global_layout_cache::clear_embed_layouts();
    crate::state::VersionRegistry::bump_embed_metadata_version();
    broadcast_settings_event(SettingsEvent::EmbedSettingsChanged);
}

pub fn set_embed_thumbnail_mode(mode: EmbedThumbnailMode) {
    {
        let mut settings = SETTINGS.lock_or_recover();
        if settings.embed_thumbnail_mode == mode {
            return;
        }
        settings.embed_thumbnail_mode = mode;
    }
    save_settings();
    crate::components::chat::canvas_renderer::global_layout_cache::clear_embed_layouts();
    crate::state::VersionRegistry::bump_embed_metadata_version();
    broadcast_settings_event(SettingsEvent::EmbedSettingsChanged);
}

pub fn set_tab_theme(theme: TabTheme) {
    SETTINGS.lock_or_recover().tab_theme = theme;
    save_settings();
    broadcast_settings_event(SettingsEvent::TabThemeChanged);
}

pub fn set_tab_overflow_mode(mode: TabOverflowMode) {
    SETTINGS.lock_or_recover().tab_overflow_mode = mode;
    save_settings();
    broadcast_settings_event(SettingsEvent::TabOverflowModeChanged);
}

pub fn set_tab_placement(placement: TabPlacement) {
    let changed = {
        let mut settings = SETTINGS.lock_or_recover();
        if settings.tab_placement == placement {
            false
        } else {
            settings.tab_placement = placement;
            true
        }
    };
    if !changed {
        return;
    }
    save_settings();
    broadcast_settings_event(SettingsEvent::TabPlacementChanged);
}

pub fn set_titlebar_density(density: TitlebarDensity) {
    let changed = {
        let mut settings = SETTINGS.lock_or_recover();
        if settings.titlebar_density == density {
            false
        } else {
            settings.titlebar_density = density;
            true
        }
    };
    if !changed {
        return;
    }
    save_settings();
    request_top_bar_state_refresh();
    broadcast_settings_event(SettingsEvent::TitlebarDensityChanged);
}

pub fn set_remove_emote_spacing(remove: bool) {
    {
        let mut settings = SETTINGS.lock_or_recover();
        if settings.remove_emote_spacing == remove {
            return;
        }
        settings.remove_emote_spacing = remove;
    }
    save_settings_debounced();
    bump_layout_settings_version();
    broadcast_settings_event(SettingsEvent::EmoteSpacingChanged);
}

pub fn set_zero_width_enabled(enabled: bool) {
    {
        let mut settings = SETTINGS.lock_or_recover();
        if settings.zero_width_enabled == enabled {
            return;
        }
        settings.zero_width_enabled = enabled;
    }
    save_settings();
    bump_layout_settings_version();
}

fn set_emote_provider_visibility(enabled: bool, select: impl Fn(&mut AppSettings) -> &mut bool) {
    {
        let mut settings = SETTINGS.lock_or_recover();
        let slot = select(&mut settings);
        if *slot == enabled {
            return;
        }
        *slot = enabled;
    }
    save_settings();
    bump_layout_settings_version();
    broadcast_settings_event(SettingsEvent::EmoteScaleChanged);
    crate::backend::emotes::schedule_provider_visibility_refresh();
}

pub fn get_enable_seventv_emotes() -> bool {
    SETTINGS.lock_or_recover().enable_seventv_emotes
}

pub fn set_enable_seventv_emotes(enabled: bool) {
    set_emote_provider_visibility(enabled, |settings| &mut settings.enable_seventv_emotes);
}

pub fn get_enable_bttv_emotes() -> bool {
    SETTINGS.lock_or_recover().enable_bttv_emotes
}

pub fn set_enable_bttv_emotes(enabled: bool) {
    set_emote_provider_visibility(enabled, |settings| &mut settings.enable_bttv_emotes);
}

pub fn get_enable_ffz_emotes() -> bool {
    SETTINGS.lock_or_recover().enable_ffz_emotes
}

pub fn set_enable_ffz_emotes(enabled: bool) {
    set_emote_provider_visibility(enabled, |settings| &mut settings.enable_ffz_emotes);
}

pub fn get_hide_emote_picker_load_more_warning() -> bool {
    SETTINGS
        .lock_or_recover()
        .hide_emote_picker_load_more_warning
}

pub fn set_hide_emote_picker_load_more_warning(enabled: bool) {
    SETTINGS
        .lock_or_recover()
        .hide_emote_picker_load_more_warning = enabled;
    save_settings();
}

pub fn get_emote_picker_animation_mode() -> EmotePickerAnimationMode {
    SETTINGS.lock_or_recover().emote_picker_animation_mode
}

pub fn set_anonymous_mode(anonymous: bool) {
    SETTINGS.lock_or_recover().anonymous_mode = anonymous;
    CACHED_ANONYMOUS_MODE.store(anonymous, Ordering::Release);
    save_settings();
    request_top_bar_state_refresh();
}

pub fn get_anonymous_mode() -> bool {
    LazyLock::force(&SETTINGS);
    CACHED_ANONYMOUS_MODE.load(Ordering::Acquire)
}

pub fn get_emoji_style() -> EmojiStyle {
    SETTINGS.lock_or_recover().emoji_style
}

pub fn set_emoji_style(style: EmojiStyle) {
    {
        let mut settings = SETTINGS.lock_or_recover();
        if settings.emoji_style == style {
            return;
        }
        settings.emoji_style = style;
    }
    save_settings();
    bump_layout_settings_version();
}

fn request_top_bar_state_refresh() {
    crate::state::ui_invalidation::request(
        crate::state::ui_invalidation::UiInvalidation::TopBarState,
    );
}

pub fn get_accounts() -> Vec<Account> {
    SETTINGS.lock_or_recover().accounts.clone()
}

pub fn get_active_account() -> Option<Account> {
    SETTINGS
        .lock_or_recover()
        .accounts
        .iter()
        .find(|a| a.is_active)
        .cloned()
}

pub fn get_refresh_token() -> Option<String> {
    SETTINGS.lock_or_recover().refresh_token.clone()
}

pub fn get_token_expires_at() -> Option<i64> {
    SETTINGS.lock_or_recover().token_expires_at
}

/// Add or update an account in the accounts list
pub fn upsert_account(account: Account) {
    let mut settings = SETTINGS.lock_or_recover();
    if account.is_active {
        for existing in &mut settings.accounts {
            existing.is_active = false;
        }
        sync_top_level_account_credentials(&mut settings, &account);
    }
    let active_username = if account.is_active {
        Some(account.username.clone())
    } else {
        settings.username.clone()
    };
    if let Some(existing) = settings
        .accounts
        .iter_mut()
        .find(|a| a.username == account.username)
    {
        *existing = account;
    } else {
        settings.accounts.push(account);
    }
    drop(settings);
    update_username_cache(active_username.as_deref());
    save_settings();
    request_top_bar_state_refresh();
}

/// Set an account as active and update top-level credentials.
/// Returns the activated account data for atomic use by callers (avoids race conditions).
pub fn set_active_account(username: &str) -> Result<Account, String> {
    let account = {
        let mut settings = SETTINGS.lock_or_recover();

        let account = settings
            .accounts
            .iter()
            .find(|a| a.username == username)
            .cloned()
            .ok_or_else(|| format!("Account '{}' not found", username))?;

        for acc in &mut settings.accounts {
            acc.is_active = false;
        }

        if let Some(acc) = settings
            .accounts
            .iter_mut()
            .find(|a| a.username == username)
        {
            acc.is_active = true;
        }

        settings.username = Some(account.username.clone());
        settings.token = runtime_access_token(&account.token);
        settings.refresh_token = account.refresh_token.clone();
        settings.token_expires_at = account.token_expires_at;

        account
    };
    update_username_cache(Some(username));
    save_settings();
    request_top_bar_state_refresh();
    Ok(account)
}

/// Remove an account from the accounts list
pub fn remove_account_from_settings(username: &str) -> Result<Option<String>, String> {
    secrets::clear_account_secrets(username);

    let switched_to = {
        let mut settings = SETTINGS.lock_or_recover();
        let was_active = settings
            .accounts
            .iter()
            .any(|a| a.username == username && a.is_active);

        settings.accounts.retain(|a| a.username != username);

        if was_active {
            if let Some(first) = settings.accounts.first().cloned() {
                if let Some(acc) = settings.accounts.first_mut() {
                    acc.is_active = true;
                }
                settings.username = Some(first.username.clone());
                settings.token = runtime_access_token(&first.token);
                settings.refresh_token = first.refresh_token;
                settings.token_expires_at = first.token_expires_at;
                Some(first.username)
            } else {
                settings.username = None;
                settings.token = None;
                settings.refresh_token = None;
                settings.token_expires_at = None;
                None
            }
        } else {
            None
        }
    };
    update_username_cache(SETTINGS.lock_or_recover().username.as_deref());
    save_settings();
    request_top_bar_state_refresh();
    Ok(switched_to)
}

/// Parse a hex color (#RRGGBB or #RGB) to RGB components (0-255)
pub fn hex_to_rgb(hex: &str) -> Option<(u8, u8, u8)> {
    hex_to_rgba(hex).map(|(r, g, b, _)| (r, g, b))
}

/// Parse a hex color into RGBA, returning the alpha channel when present.
///
/// Accepts `#AARRGGBB` (alpha first) in addition to `#RGB` and
/// `#RRGGBB`. The returned alpha is `Some` only for the 8-digit form; callers
/// substitute a per-highlight-type default alpha when it is `None`.
pub fn hex_to_rgba(hex: &str) -> Option<(u8, u8, u8, Option<u8>)> {
    let hex = hex.trim_start_matches('#');
    if !hex.is_ascii() {
        return None;
    }
    match hex.len() {
        3 => {
            let r = u8::from_str_radix(&hex[0..1], 16).ok()? * 17;
            let g = u8::from_str_radix(&hex[1..2], 16).ok()? * 17;
            let b = u8::from_str_radix(&hex[2..3], 16).ok()? * 17;
            Some((r, g, b, None))
        }
        6 => {
            let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
            let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
            let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
            Some((r, g, b, None))
        }
        8 => {
            let a = u8::from_str_radix(&hex[0..2], 16).ok()?;
            let r = u8::from_str_radix(&hex[2..4], 16).ok()?;
            let g = u8::from_str_radix(&hex[4..6], 16).ok()?;
            let b = u8::from_str_radix(&hex[6..8], 16).ok()?;
            Some((r, g, b, Some(a)))
        }
        _ => None,
    }
}

pub fn get_mention_color() -> String {
    SETTINGS.lock_or_recover().mention_color.clone()
}

pub fn set_mention_color(color: String) {
    SETTINGS.lock_or_recover().mention_color = color;
    save_settings_debounced();
    broadcast_settings_event(SettingsEvent::HighlightColorChanged);
}

pub fn get_whisper_color() -> String {
    SETTINGS.lock_or_recover().whisper_color.clone()
}

pub fn set_whisper_color(color: String) {
    SETTINGS.lock_or_recover().whisper_color = color;
    save_settings_debounced();
    broadcast_settings_event(SettingsEvent::HighlightColorChanged);
}

pub fn get_subscription_color() -> String {
    SETTINGS.lock_or_recover().subscription_color.clone()
}

pub fn set_subscription_color(color: String) {
    SETTINGS.lock_or_recover().subscription_color = color;
    save_settings_debounced();
    broadcast_settings_event(SettingsEvent::HighlightColorChanged);
}

pub fn get_first_message_color() -> String {
    SETTINGS.lock_or_recover().first_message_color.clone()
}

pub fn set_first_message_color(color: String) {
    SETTINGS.lock_or_recover().first_message_color = color;
    save_settings_debounced();
    broadcast_settings_event(SettingsEvent::HighlightColorChanged);
}

pub fn get_thread_highlight_color() -> String {
    SETTINGS.lock_or_recover().thread_highlight_color.clone()
}

pub fn set_thread_highlight_color(color: String) {
    SETTINGS.lock_or_recover().thread_highlight_color = color;
    save_settings_debounced();
    broadcast_settings_event(SettingsEvent::HighlightColorChanged);
}

pub fn get_flash_on_mention() -> bool {
    SETTINGS.lock_or_recover().flash_on_mention
}

pub fn set_flash_on_mention(enabled: bool) {
    SETTINGS.lock_or_recover().flash_on_mention = enabled;
    save_settings();
}

pub fn get_show_mention_in_title() -> bool {
    SETTINGS.lock_or_recover().show_mention_in_title
}

pub fn set_show_mention_in_title(enabled: bool) {
    SETTINGS.lock_or_recover().show_mention_in_title = enabled;
    save_settings();
    request_top_bar_state_refresh();
}

pub fn get_thread_highlight_enabled() -> bool {
    SETTINGS.lock_or_recover().thread_highlight_enabled
}

pub fn set_thread_highlight_enabled(enabled: bool) {
    SETTINGS.lock_or_recover().thread_highlight_enabled = enabled;
    save_settings();
}

pub fn get_thread_highlight_sound_enabled() -> bool {
    SETTINGS.lock_or_recover().thread_highlight_sound_enabled
}

pub fn set_thread_highlight_sound_enabled(enabled: bool) {
    SETTINGS.lock_or_recover().thread_highlight_sound_enabled = enabled;
    save_settings();
}

pub fn get_thread_highlight_show_in_mentions() -> bool {
    SETTINGS.lock_or_recover().thread_highlight_show_in_mentions
}

pub fn set_thread_highlight_show_in_mentions(enabled: bool) {
    SETTINGS.lock_or_recover().thread_highlight_show_in_mentions = enabled;
    save_settings();
}

pub fn get_enable_twitch_blocked_users() -> bool {
    LazyLock::force(&SETTINGS);
    CACHED_ENABLE_TWITCH_BLOCKED_USERS.load(Ordering::Acquire)
}

pub fn set_enable_twitch_blocked_users(enabled: bool) {
    let visibility_commit = crate::backend::blocked_users::visibility_commit_guard();
    {
        let mut settings = SETTINGS.lock_or_recover();
        if settings.enable_twitch_blocked_users == enabled {
            return;
        }
        settings.enable_twitch_blocked_users = enabled;
    }
    CACHED_ENABLE_TWITCH_BLOCKED_USERS.store(enabled, Ordering::Release);
    let (changed_channels, visibility_effects) = if enabled {
        (
            crate::backend::irc::apply_blocked_user_filter_to_buffers_visibility_commit_held(),
            None,
        )
    } else {
        (
            Default::default(),
            Some(
                crate::backend::irc::finalize_pending_blocked_visibility_commit_held(
                    crate::backend::blocked_users::current_auth_epoch_visibility_commit_held(),
                ),
            ),
        )
    };
    drop(visibility_commit);
    crate::backend::irc::dispatch_blocked_channel_refreshes(changed_channels);
    if let Some(effects) = visibility_effects {
        crate::backend::irc::dispatch_blocked_visibility_effects(effects);
    }
    save_settings();
    broadcast_settings_event(SettingsEvent::FiltersChanged);
}

pub fn get_show_blocked_users_messages() -> BlockedUserVisibility {
    LazyLock::force(&SETTINGS);
    BlockedUserVisibility::from_u64(CACHED_SHOW_BLOCKED_USERS_MESSAGES.load(Ordering::Acquire))
}

pub fn set_show_blocked_users_messages(visibility: BlockedUserVisibility) {
    let visibility_commit = crate::backend::blocked_users::visibility_commit_guard();
    {
        let mut settings = SETTINGS.lock_or_recover();
        if settings.show_blocked_users_messages == visibility {
            return;
        }
        settings.show_blocked_users_messages = visibility;
    }
    CACHED_SHOW_BLOCKED_USERS_MESSAGES.store(visibility.as_u64(), Ordering::Release);
    let changed_channels =
        crate::backend::irc::apply_blocked_user_filter_to_buffers_visibility_commit_held();
    drop(visibility_commit);
    crate::backend::irc::dispatch_blocked_channel_refreshes(changed_channels);
    save_settings();
    broadcast_settings_event(SettingsEvent::FiltersChanged);
}

pub fn get_exclude_user_messages_from_filter() -> bool {
    LazyLock::force(&SETTINGS);
    CACHED_EXCLUDE_USER_MESSAGES_FROM_FILTER.load(Ordering::Acquire)
}

pub fn set_exclude_user_messages_from_filter(excluded: bool) {
    {
        let mut settings = SETTINGS.lock_or_recover();
        if settings.exclude_user_messages_from_filter == excluded {
            return;
        }
        settings.exclude_user_messages_from_filter = excluded;
    }
    CACHED_EXCLUDE_USER_MESSAGES_FROM_FILTER.store(excluded, Ordering::Release);
    save_settings();
    broadcast_settings_event(SettingsEvent::FiltersChanged);
}

/// Monotonic version stamp for the filter rule set. Bumped by every filter
/// mutation so the ignore-phrase replace cache can invalidate its compiled
/// regexes without recompiling on every message.
pub fn filters_version() -> u64 {
    FILTERS_VERSION.load(Ordering::Acquire)
}

fn bump_filters_version() {
    FILTERS_VERSION.fetch_add(1, Ordering::Release);
}

pub fn get_filters() -> Vec<FilterRule> {
    SETTINGS.lock_or_recover().filters.clone()
}

pub fn add_filter(filter: FilterRule) {
    SETTINGS.lock_or_recover().filters.push(filter);
    bump_filters_version();
    save_settings();
}

pub fn update_filter(id: &str, updated: FilterRule) {
    let mut settings = SETTINGS.lock_or_recover();
    if let Some(existing) = settings.filters.iter_mut().find(|f| f.id == id) {
        *existing = updated;
    }
    drop(settings);
    bump_filters_version();
    save_settings();
}

pub fn remove_filter(id: &str) {
    SETTINGS.lock_or_recover().filters.retain(|f| f.id != id);
    bump_filters_version();
    save_settings();
}

pub fn get_highlight_blacklist_users() -> Vec<HighlightBlacklistUser> {
    SETTINGS.lock_or_recover().highlight_blacklist_users.clone()
}

pub fn add_highlight_blacklist_user(mut user: HighlightBlacklistUser) {
    let pattern = user.pattern.trim();
    if pattern.is_empty() {
        return;
    }

    if user.id.trim().is_empty() {
        user.id = format!("highlight-blacklist-{}", crate::utils::unix_now_ms());
    }
    user.pattern = pattern.to_string();

    let mut settings = SETTINGS.lock_or_recover();
    if settings
        .highlight_blacklist_users
        .iter()
        .any(|existing| existing.id == user.id)
    {
        return;
    }
    settings.highlight_blacklist_users.push(user);
    drop(settings);
    save_settings();
}

pub fn update_highlight_blacklist_user(id: &str, mut updated: HighlightBlacklistUser) {
    let pattern = updated.pattern.trim();
    if pattern.is_empty() {
        return;
    }

    updated.id = id.to_string();
    updated.pattern = pattern.to_string();

    let mut settings = SETTINGS.lock_or_recover();
    if let Some(existing) = settings
        .highlight_blacklist_users
        .iter_mut()
        .find(|entry| entry.id == id)
    {
        *existing = updated;
    }
    drop(settings);
    save_settings();
}

pub fn remove_highlight_blacklist_user(id: &str) {
    SETTINGS
        .lock_or_recover()
        .highlight_blacklist_users
        .retain(|entry| entry.id != id);
    save_settings();
}

pub fn is_highlight_blacklisted_user(username: &str) -> bool {
    if username.trim().is_empty() {
        return false;
    }

    let entries = SETTINGS.lock_or_recover().highlight_blacklist_users.clone();
    entries.iter().any(|entry| entry.matches_username(username))
}

pub fn get_highlight_rules() -> Vec<HighlightRule> {
    SETTINGS.lock_or_recover().highlight_rules.clone()
}

pub fn add_highlight_rule(mut rule: HighlightRule) {
    let pattern = rule.pattern.trim();
    if pattern.is_empty() {
        return;
    }

    if rule.id.trim().is_empty() {
        rule.id = default_highlight_rule_id();
    }
    rule.pattern = pattern.to_string();
    rule.sound_path = normalize_optional_path(rule.sound_path.take())
        .filter(|path| !rejected_new_sound_path(path));

    let mut settings = SETTINGS.lock_or_recover();
    if settings
        .highlight_rules
        .iter()
        .any(|existing| existing.id == rule.id)
    {
        return;
    }
    settings.highlight_rules.push(rule);
    drop(settings);
    highlight_rules_changed();
}

pub fn update_highlight_rule(id: &str, mut updated: HighlightRule) {
    let pattern = updated.pattern.trim();
    if pattern.is_empty() {
        return;
    }

    updated.id = id.to_string();
    updated.pattern = pattern.to_string();
    updated.sound_path = normalize_optional_path(updated.sound_path.take());

    let mut settings = SETTINGS.lock_or_recover();
    if let Some(existing) = settings
        .highlight_rules
        .iter_mut()
        .find(|entry| entry.id == id)
    {
        if updated.sound_path != existing.sound_path
            && updated
                .sound_path
                .as_deref()
                .is_some_and(rejected_new_sound_path)
        {
            updated.sound_path = existing.sound_path.clone();
        }
        *existing = updated;
    }
    drop(settings);
    highlight_rules_changed();
}

/// A newly chosen highlight sound on a network share or device is refused.
fn rejected_new_sound_path(path: &str) -> bool {
    let rejected = is_remote_or_device_path(path);
    if rejected {
        crate::log_persist!(
            warn,
            "Ignoring highlight sound: network (UNC) and device paths are not allowed"
        );
    }
    rejected
}

pub fn remove_highlight_rule(id: &str) {
    let mut settings = SETTINGS.lock_or_recover();
    let before = settings.highlight_rules.len();
    settings.highlight_rules.retain(|entry| entry.id != id);
    let removed = settings.highlight_rules.len() != before;
    drop(settings);
    if removed {
        highlight_rules_changed();
    }
}

fn highlight_rules_changed() {
    {
        let settings = SETTINGS.lock_or_recover();
        highlight_matcher::publish(&settings.highlight_rules);
    }
    save_settings();
}

pub fn get_nickname_rules() -> Vec<NicknameRule> {
    SETTINGS.lock_or_recover().nicknames.clone()
}

fn nicknames_changed() {
    {
        let settings = SETTINGS.lock_or_recover();
        publish_compiled_nickname_rules(&settings.nicknames);
    }
    save_settings();
    bump_layout_settings_version();
    broadcast_settings_event(SettingsEvent::NicknamesChanged);
}

pub fn add_nickname_rule(mut rule: NicknameRule) {
    let pattern = rule.pattern.trim();
    rule.user_id = rule
        .user_id
        .as_deref()
        .map(str::trim)
        .filter(|user_id| !user_id.is_empty())
        .map(ToOwned::to_owned);
    if pattern.is_empty() && rule.user_id.is_none() {
        return;
    }
    if rule.id.trim().is_empty() {
        rule.id = default_nickname_rule_id();
    }
    rule.pattern = pattern.to_string();
    SETTINGS.lock_or_recover().nicknames.push(rule);
    nicknames_changed();
}

pub fn update_nickname_rule(id: &str, mut updated: NicknameRule) {
    let pattern = updated.pattern.trim();
    updated.user_id = updated
        .user_id
        .as_deref()
        .map(str::trim)
        .filter(|user_id| !user_id.is_empty())
        .map(ToOwned::to_owned);
    if pattern.is_empty() && updated.user_id.is_none() {
        return;
    }
    updated.id = id.to_string();
    updated.pattern = pattern.to_string();

    let mut settings = SETTINGS.lock_or_recover();
    if let Some(entry) = settings.nicknames.iter_mut().find(|entry| entry.id == id) {
        *entry = updated;
        drop(settings);
        nicknames_changed();
    }
}

pub fn remove_nickname_rule(id: &str) {
    let mut settings = SETTINGS.lock_or_recover();
    let before = settings.nicknames.len();
    settings.nicknames.retain(|entry| entry.id != id);
    let removed = settings.nicknames.len() != before;
    drop(settings);
    if removed {
        nicknames_changed();
    }
}

pub fn get_emote_display_rules() -> Vec<EmoteDisplayRule> {
    SETTINGS.lock_or_recover().emote_display_rules.clone()
}

/// Republish the emote display rules after the persisted list changed and
/// re-lay out chat. Emote and resolver versions are deliberately untouched:
/// the rules change presentation, not emote resolution.
pub fn emote_display_rules_changed() {
    {
        let settings = SETTINGS.lock_or_recover();
        publish_compiled_emote_display_rules(&settings.emote_display_rules);
    }
    save_settings();
    bump_layout_settings_version();
    broadcast_settings_event(SettingsEvent::EmoteDisplayRulesChanged);
}

/// Apply rule changes in order. Returns the changes that undo them, in the
/// order to apply them, or an empty list when nothing changed.
pub fn apply_emote_display_rule_changes(changes: &[EmoteRuleChange]) -> Vec<EmoteRuleChange> {
    let mut undo = {
        let mut settings = SETTINGS.lock_or_recover();
        changes
            .iter()
            .filter_map(|change| {
                echo_core::emote_display_rules::apply_change(
                    &mut settings.emote_display_rules,
                    change,
                )
            })
            .collect::<Vec<_>>()
    };
    if !undo.is_empty() {
        undo.reverse();
        emote_display_rules_changed();
    }
    undo
}

/// Set one flag of one listed rule under the settings lock, keeping the row
/// even when both flags end up off and leaving the other flag as stored.
pub fn set_emote_display_rule_flag(
    emote: &echo_core::emote_preferences::EmoteRef,
    scope: &EmoteRuleScope,
    flag: EmoteDisplayFlag,
    value: bool,
) {
    let changed = echo_core::emote_display_rules::set_rule_flag(
        &mut SETTINGS.lock_or_recover().emote_display_rules,
        emote,
        scope,
        flag,
        value,
    );
    if changed {
        emote_display_rules_changed();
    }
}

pub fn remove_emote_display_rule(
    emote: &echo_core::emote_preferences::EmoteRef,
    scope: &EmoteRuleScope,
) {
    let removed = echo_core::emote_display_rules::remove_rule(
        &mut SETTINGS.lock_or_recover().emote_display_rules,
        emote,
        scope,
    );
    if removed {
        emote_display_rules_changed();
    }
}

pub fn match_highlight_rule_for_targets(
    message: &str,
    sender_login: &str,
    badges: &[String],
    targets: &[HighlightRuleTarget],
) -> Option<MatchedHighlightRule> {
    let current_user = get_username_lowercase();
    highlight_matcher::match_rule_for_targets(message, sender_login, badges, targets, &current_user)
}

/// Sound/alert effects merged across every matching highlight rule.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct HighlightRuleEffects {
    pub play_sound: bool,
    pub alert: bool,
    pub sound_path: Option<String>,
}

/// Effect union across ALL matching highlight rules, in the same target/rule
/// order as [`match_highlight_rule_for_targets`]. The first matching rule that
/// enables `play_sound` supplies the sound (including its custom path);
/// `alert` is set by any matching rule that sets it, so a colored
/// higher-priority rule never silences a sounding lower-priority rule.
/// Returns `None` when no rule matches at all.
pub fn match_highlight_rule_effects_for_targets(
    message: &str,
    sender_login: &str,
    badges: &[String],
    targets: &[HighlightRuleTarget],
) -> Option<HighlightRuleEffects> {
    let current_user = get_username_lowercase();
    highlight_matcher::match_effects_for_targets(
        message,
        sender_login,
        badges,
        targets,
        &current_user,
    )
}

/// Highlight rules compiled once for a one-off evaluation, such as the
/// Highlights tester. Matching runs the same compiled matchers as
/// [`match_highlight_rule_for_targets`] and
/// [`match_highlight_rule_effects_for_targets`], but the caller states whether
/// the sender is the signed-in user instead of it being derived from the login.
pub struct HighlightRuleSet {
    compiled: Arc<highlight_matcher::CompiledHighlightRules>,
}

impl HighlightRuleSet {
    /// The rules live chat is matching against right now.
    pub fn published() -> Self {
        Self {
            compiled: highlight_matcher::published(),
        }
    }

    /// `rules` compiled exactly as a publish would compile them.
    pub fn from_rules(rules: &[HighlightRule]) -> Self {
        Self {
            compiled: highlight_matcher::compile(rules),
        }
    }

    /// First matching rule across `targets`, in target then rule order.
    /// Phrase rules never match a message from the signed-in user.
    pub fn match_for_targets(
        &self,
        message: &str,
        sender_login: &str,
        badges: &[String],
        targets: &[HighlightRuleTarget],
        is_self_sender: bool,
    ) -> Option<MatchedHighlightRule> {
        self.compiled
            .first_match(message, sender_login, badges, targets, is_self_sender)
    }

    /// Sound/alert effects merged across every matching rule.
    pub fn effects_for_targets(
        &self,
        message: &str,
        sender_login: &str,
        badges: &[String],
        targets: &[HighlightRuleTarget],
        is_self_sender: bool,
    ) -> Option<HighlightRuleEffects> {
        self.compiled
            .effects(message, sender_login, badges, targets, is_self_sender)
    }
}

/// The saved highlight rule that produced `origin`, a reason recorded on a
/// chat row. Rows keep the matched pattern, sender or badge rather than a rule
/// id, so this returns `None` when no saved rule could have produced it (it was
/// edited or removed since), or when several phrase rules share the pattern
/// but match differently and the recorded color does not tell them apart.
pub fn find_highlight_rule_for_origin(
    origin: echo_core::highlight_explanation::HighlightRuleOrigin<'_>,
) -> Option<HighlightRule> {
    let rules = get_highlight_rules();
    let mut candidates: Vec<&HighlightRule> = rules
        .iter()
        .filter(|rule| highlight_matcher::rule_produces_origin(rule, origin))
        .collect();
    /// Keep only the candidates that pass, unless that would drop all of them.
    fn narrow(candidates: &mut Vec<&HighlightRule>, keep: impl Fn(&HighlightRule) -> bool) {
        if candidates.len() > 1 && candidates.iter().any(|rule| keep(rule)) {
            candidates.retain(|rule| keep(rule));
        }
    }
    narrow(&mut candidates, |rule| {
        rule.color.as_deref() == origin.color
    });
    narrow(&mut candidates, |rule| rule.enabled);
    use echo_core::highlight_explanation::HighlightRuleKind;
    // Rules match in list order. Every user or badge candidate accepts the
    // recorded sender or badge, and phrase candidates with the same pattern
    // and flags match the same messages, so in those cases the first one fired.
    match (origin.kind, candidates.as_slice()) {
        (_, [rule]) => Some((*rule).clone()),
        (HighlightRuleKind::User | HighlightRuleKind::Badge, [first, ..]) => Some((*first).clone()),
        (HighlightRuleKind::Phrase, [first, rest @ ..])
            if rest.iter().all(|rule| {
                rule.regex == first.regex && rule.case_sensitive == first.case_sensitive
            }) =>
        {
            Some((*first).clone())
        }
        _ => None,
    }
}

/// Export all filter rules as a JSON string.
/// Returns Ok(json) or Err(message).
pub fn export_filters_to_json() -> Result<String, String> {
    let filters = SETTINGS.lock_or_recover().filters.clone();
    serde_json::to_string_pretty(&filters).map_err(|e| format!("Export failed: {e}"))
}

/// Import filter rules from a JSON string (replaces all existing filters).
/// The JSON must be an array of FilterRule objects.
/// Returns Ok(count) with the number of imported rules, or Err(message).
pub fn import_filters_from_json(json: &str) -> Result<usize, String> {
    let imported: Vec<FilterRule> =
        serde_json::from_str(json).map_err(|e| format!("Invalid filter JSON: {e}"))?;
    let count = imported.len();
    SETTINGS.lock_or_recover().filters = imported;
    bump_filters_version();
    save_settings();
    Ok(count)
}

pub fn get_mod_buttons() -> Vec<crate::backend::mod_buttons::ModButtonConfig> {
    SETTINGS.lock_or_recover().mod_buttons.clone()
}

pub fn add_mod_button(button: crate::backend::mod_buttons::ModButtonConfig) {
    SETTINGS.lock_or_recover().mod_buttons.push(button);
    save_settings();
    bump_layout_settings_version();
}

pub fn update_mod_button(index: usize, button: crate::backend::mod_buttons::ModButtonConfig) {
    let mut settings = SETTINGS.lock_or_recover();
    if let Some(existing) = settings.mod_buttons.get_mut(index) {
        *existing = button;
    }
    drop(settings);
    save_settings();
    bump_layout_settings_version();
}

pub fn remove_mod_button(index: usize) {
    let mut settings = SETTINGS.lock_or_recover();
    if index < settings.mod_buttons.len() {
        settings.mod_buttons.remove(index);
    }
    drop(settings);
    save_settings();
    bump_layout_settings_version();
}

pub fn reset_mod_buttons_to_defaults() {
    SETTINGS.lock_or_recover().mod_buttons = crate::backend::mod_buttons::default_mod_buttons();
    save_settings();
    bump_layout_settings_version();
}
pub fn get_unified_input_mode() -> bool {
    SETTINGS.lock_or_recover().unified_input_mode
}

pub fn set_unified_input_mode(unified: bool) {
    SETTINGS.lock_or_recover().unified_input_mode = unified;
    save_settings();
}

pub fn get_compact_split_header() -> bool {
    SETTINGS.lock_or_recover().compact_split_header
}

pub fn set_compact_split_header(compact: bool) {
    {
        let mut settings = SETTINGS.lock_or_recover();
        if settings.compact_split_header == compact {
            return;
        }
        settings.compact_split_header = compact;
    }
    save_settings();
    broadcast_settings_event(SettingsEvent::CosmeticSettingsChanged);
}

pub fn get_show_send_button() -> bool {
    SETTINGS.lock_or_recover().show_send_button
}

pub fn set_show_send_button(show: bool) {
    SETTINGS.lock_or_recover().show_send_button = show;
    save_settings();
}

/// Advanced/developer controls are opt-in so the default Settings surface
/// stays focused on ordinary chat use. The underlying controls remain fully
/// available when a user explicitly reveals them.
pub fn get_developer_mode() -> bool {
    SETTINGS.lock_or_recover().developer_mode
}

pub fn set_developer_mode(enabled: bool) {
    SETTINGS.lock_or_recover().developer_mode = enabled;
    save_settings();
}

pub fn get_settings_navigation_prefs() -> SettingsNavigationPrefs {
    SETTINGS.lock_or_recover().settings_navigation.clone()
}

/// Remember the last location outside the developer disclosure.
pub fn set_settings_navigation_last_standard(location: Option<PersistedSettingsLocation>) -> bool {
    let changed = {
        let mut settings = SETTINGS.lock_or_recover();
        let prefs = &mut settings.settings_navigation;
        if prefs.last_standard == location && prefs.is_initialized() {
            false
        } else {
            prefs.version = SETTINGS_NAVIGATION_PREFS_VERSION;
            prefs.last_standard = location;
            true
        }
    };

    if changed {
        save_settings_debounced();
    }
    changed
}

/// Remember the last developer-only location separately, so turning the
/// disclosure off restores the last standard page instead.
pub fn set_settings_navigation_last_developer(location: Option<PersistedSettingsLocation>) -> bool {
    let changed = {
        let mut settings = SETTINGS.lock_or_recover();
        let prefs = &mut settings.settings_navigation;
        if prefs.last_developer == location && prefs.is_initialized() {
            false
        } else {
            prefs.version = SETTINGS_NAVIGATION_PREFS_VERSION;
            prefs.last_developer = location;
            true
        }
    };

    if changed {
        save_settings_debounced();
    }
    changed
}

pub fn get_tab_complete_usernames() -> bool {
    crate::benchmark_ui_fixture::is_enabled() || SETTINGS.lock_or_recover().tab_complete_usernames
}

pub fn set_tab_complete_usernames(enabled: bool) {
    SETTINGS.lock_or_recover().tab_complete_usernames = enabled;
    save_settings();
}

pub fn get_mention_users_with_comma() -> bool {
    crate::benchmark_ui_fixture::is_enabled() || SETTINGS.lock_or_recover().mention_users_with_comma
}

pub fn set_mention_users_with_comma(enabled: bool) {
    SETTINGS.lock_or_recover().mention_users_with_comma = enabled;
    save_settings();
}

pub fn get_slash_commands_enabled() -> bool {
    SETTINGS.lock_or_recover().slash_commands_enabled
}

pub fn set_slash_commands_enabled(enabled: bool) {
    SETTINGS.lock_or_recover().slash_commands_enabled = enabled;
    save_settings();
}

pub fn get_command_autocomplete_enabled() -> bool {
    SETTINGS.lock_or_recover().command_autocomplete_enabled
}

pub fn set_command_autocomplete_enabled(enabled: bool) {
    SETTINGS.lock_or_recover().command_autocomplete_enabled = enabled;
    save_settings();
}

pub fn get_show_command_descriptions() -> bool {
    SETTINGS.lock_or_recover().show_command_descriptions
}

pub fn set_show_command_descriptions(enabled: bool) {
    SETTINGS.lock_or_recover().show_command_descriptions = enabled;
    save_settings();
}
pub fn get_custom_commands() -> Vec<CustomCommand> {
    SETTINGS.lock_or_recover().custom_commands.clone()
}

pub fn add_custom_command(command: CustomCommand) {
    SETTINGS.lock_or_recover().custom_commands.push(command);
    save_settings();
}

pub fn update_custom_command(id: &str, updated: CustomCommand) {
    let mut settings = SETTINGS.lock_or_recover();
    if let Some(existing) = settings.custom_commands.iter_mut().find(|c| c.id == id) {
        *existing = updated;
    }
    drop(settings);
    save_settings();
}

pub fn remove_custom_command(id: &str) {
    SETTINGS
        .lock_or_recover()
        .custom_commands
        .retain(|command| command.id != id);
    save_settings();
}

pub fn export_custom_commands_to_json() -> Result<String, String> {
    let commands = SETTINGS.lock_or_recover().custom_commands.clone();
    serde_json::to_string_pretty(&commands).map_err(|e| format!("Export failed: {e}"))
}

pub fn import_custom_commands_from_json(json: &str) -> Result<usize, String> {
    let imported: Vec<CustomCommand> =
        serde_json::from_str(json).map_err(|e| format!("Invalid command JSON: {e}"))?;
    let count = imported.len();
    SETTINGS.lock_or_recover().custom_commands = imported;
    save_settings();
    Ok(count)
}

/// The settings collections an importer appends to.
#[derive(Clone, Debug, Default)]
pub struct ImportedCollections {
    pub highlight_rules: Vec<HighlightRule>,
    pub highlight_blacklist_users: Vec<HighlightBlacklistUser>,
    pub filters: Vec<FilterRule>,
    pub nicknames: Vec<NicknameRule>,
    pub custom_commands: Vec<CustomCommand>,
    pub mod_buttons: Vec<crate::backend::mod_buttons::ModButtonConfig>,
}

impl ImportedCollections {
    pub fn is_empty(&self) -> bool {
        self.highlight_rules.is_empty()
            && self.highlight_blacklist_users.is_empty()
            && self.filters.is_empty()
            && self.nicknames.is_empty()
            && self.custom_commands.is_empty()
            && self.mod_buttons.is_empty()
    }
}

fn extend_collections(settings: &mut AppSettings, additions: ImportedCollections) {
    settings.highlight_rules.extend(additions.highlight_rules);
    settings
        .highlight_blacklist_users
        .extend(additions.highlight_blacklist_users);
    settings.filters.extend(additions.filters);
    settings.nicknames.extend(additions.nicknames);
    settings.custom_commands.extend(additions.custom_commands);
    settings.mod_buttons.extend(additions.mod_buttons);
}

/// The live settings with `additions` appended, for writing to disk before
/// the live settings change. Nothing in memory is touched.
pub fn settings_with_collections(additions: &ImportedCollections) -> AppSettings {
    let mut snapshot = SETTINGS.lock_or_recover().clone();
    extend_collections(&mut snapshot, additions.clone());
    snapshot
}

/// Append imported entries to the live settings in one step, publish their
/// compiled matchers the way the individual setters do, and queue a save.
/// Importers call this only after their staged settings reached the disk.
pub fn append_collections(additions: ImportedCollections) {
    {
        let mut settings = SETTINGS.lock_or_recover();
        extend_collections(&mut settings, additions);
        highlight_matcher::publish(&settings.highlight_rules);
        publish_compiled_nickname_rules(&settings.nicknames);
    }
    bump_filters_version();
    bump_layout_settings_version();
    broadcast_settings_event(SettingsEvent::NicknamesChanged);
    broadcast_settings_event(SettingsEvent::FiltersChanged);
    save_settings();
}

pub fn get_chatterino_import_prompt_dismissed() -> bool {
    SETTINGS
        .lock_or_recover()
        .chatterino_import_prompt_dismissed
}

pub fn set_chatterino_import_prompt_dismissed(dismissed: bool) {
    let mut settings = SETTINGS.lock_or_recover();
    if settings.chatterino_import_prompt_dismissed == dismissed {
        return;
    }
    settings.chatterino_import_prompt_dismissed = dismissed;
    drop(settings);
    save_settings();
}

/// Get all hotkey bindings
pub fn get_hotkeys() -> HashMap<String, HotkeyBinding> {
    SETTINGS.lock_or_recover().hotkeys.clone()
}

/// Set a single hotkey binding
pub fn set_hotkey(action: String, binding: HotkeyBinding) {
    SETTINGS.lock_or_recover().hotkeys.insert(action, binding);
    save_settings();
}

/// Remove a single hotkey binding
pub fn clear_hotkey(action: &str) {
    SETTINGS.lock_or_recover().hotkeys.remove(action);
    save_settings();
}

/// Reset all hotkeys to defaults
pub fn reset_hotkeys_to_default() {
    SETTINGS.lock_or_recover().hotkeys = default_hotkeys();
    save_settings();
}

/// Check if message fade-in animation is enabled
pub fn get_show_join_part_messages() -> bool {
    SETTINGS.lock_or_recover().show_join_part_messages
}

pub fn set_show_join_part_messages(enabled: bool) {
    SETTINGS.lock_or_recover().show_join_part_messages = enabled;
    save_settings();
}

/// Check if alternating (zebra) message backgrounds are enabled
pub fn get_alternate_message_backgrounds() -> bool {
    CACHED_ALTERNATE_MESSAGE_BACKGROUNDS.load(Ordering::Acquire)
}

pub fn set_alternate_message_backgrounds(enabled: bool) {
    SETTINGS.lock_or_recover().alternate_message_backgrounds = enabled;
    CACHED_ALTERNATE_MESSAGE_BACKGROUNDS.store(enabled, Ordering::Release);
    save_settings();
}

/// Check if highlight flash animation is enabled
pub fn get_highlight_flash_enabled() -> bool {
    SETTINGS.lock_or_recover().highlight_flash_enabled
}

/// Whether non-content interface animations should render statically.
///
/// This is an explicit Echo preference. GPUI currently exposes no platform
/// preference bridge here, so it deliberately does not claim to follow the OS
/// reduced-motion setting.
pub fn get_reduce_decorative_motion() -> bool {
    SETTINGS.lock_or_recover().reduce_decorative_motion
}

pub fn get_animate_emotes() -> bool {
    SETTINGS.lock_or_recover().animate_emotes
}

pub fn set_animate_emotes(enabled: bool) {
    let changed = {
        let mut settings = SETTINGS.lock_or_recover();
        if settings.animate_emotes == enabled {
            false
        } else {
            settings.animate_emotes = enabled;
            true
        }
    };
    if !changed {
        return;
    }
    crate::emote_renderer::set_animation_enabled(enabled);
    save_settings();
    broadcast_settings_event(SettingsEvent::AnimationPerformanceChanged);
}

/// Set the application-wide decorative-motion policy.
pub fn set_reduce_decorative_motion(enabled: bool) {
    {
        let mut settings = SETTINGS.lock_or_recover();
        if settings.reduce_decorative_motion == enabled {
            return;
        }
        settings.reduce_decorative_motion = enabled;
    }
    save_settings();
    broadcast_settings_event(SettingsEvent::AnimationPerformanceChanged);
}

/// Set highlight flash animation enabled
pub fn set_highlight_flash_enabled(enabled: bool) {
    SETTINGS.lock_or_recover().highlight_flash_enabled = enabled;
    save_settings();
}

/// Check if smooth scroll animation is enabled
pub fn get_smooth_scroll_enabled() -> bool {
    SETTINGS.lock_or_recover().smooth_scroll_enabled
}

/// Set smooth scroll animation enabled
pub fn set_smooth_scroll_enabled(enabled: bool) {
    SETTINGS.lock_or_recover().smooth_scroll_enabled = enabled;
    save_settings();
}

/// Whether bursts of live messages are revealed at a steady pace.
pub fn get_paced_chat_reveal() -> bool {
    SETTINGS.lock_or_recover().paced_chat_reveal
}

/// Set whether bursts of live messages are revealed at a steady pace.
pub fn set_paced_chat_reveal(enabled: bool) {
    SETTINGS.lock_or_recover().paced_chat_reveal = enabled;
    save_settings();
}

/// Get scroll animation duration in milliseconds
/// Returns 150ms by default
pub fn get_scroll_animation_duration_ms() -> u32 {
    SETTINGS.lock_or_recover().scroll_animation_duration_ms
}

pub use super::badges_core::{ExternalBadgeProvider, external_badge_provider_for_key};

pub fn should_render_badge_key(badge_key: &str) -> bool {
    let Some(provider) = external_badge_provider_for_key(badge_key) else {
        return true;
    };

    let settings = SETTINGS.lock_or_recover();
    match provider {
        ExternalBadgeProvider::Ffz => settings.show_ffz_badges,
        ExternalBadgeProvider::Bttv => settings.show_bttv_badges,
        ExternalBadgeProvider::BttvPro => settings.show_bttv_pro_badges,
        ExternalBadgeProvider::FfzBot => settings.show_ffz_bot_badges,
    }
}

fn set_external_badge_visibility(show: bool, select: impl FnOnce(&mut AppSettings) -> &mut bool) {
    {
        let mut settings = SETTINGS.lock_or_recover();
        let current = select(&mut settings);
        if *current == show {
            return;
        }
        *current = show;
    }
    save_settings();
    bump_layout_settings_version();
    broadcast_settings_event(SettingsEvent::CosmeticSettingsChanged);
}

pub fn get_show_ffz_badges() -> bool {
    SETTINGS.lock_or_recover().show_ffz_badges
}

pub fn set_show_ffz_badges(show: bool) {
    set_external_badge_visibility(show, |settings| &mut settings.show_ffz_badges);
}

pub fn get_show_bttv_badges() -> bool {
    SETTINGS.lock_or_recover().show_bttv_badges
}

pub fn set_show_bttv_badges(show: bool) {
    set_external_badge_visibility(show, |settings| &mut settings.show_bttv_badges);
}

/// Check if BTTV Pro badges are visible
pub fn get_show_bttv_pro_badges() -> bool {
    SETTINGS.lock_or_recover().show_bttv_pro_badges
}

/// Set BTTV Pro badge visibility
pub fn set_show_bttv_pro_badges(show: bool) {
    set_external_badge_visibility(show, |settings| &mut settings.show_bttv_pro_badges);
}

/// Check if FFZ Bot badges are visible
pub fn get_show_ffz_bot_badges() -> bool {
    SETTINGS.lock_or_recover().show_ffz_bot_badges
}

/// Set FFZ Bot badge visibility
pub fn set_show_ffz_bot_badges(show: bool) {
    set_external_badge_visibility(show, |settings| &mut settings.show_ffz_bot_badges);
}

pub fn get_show_seventv_name_paints() -> bool {
    SETTINGS.lock_or_recover().show_seventv_name_paints
}

pub fn set_show_seventv_name_paints(show: bool) {
    {
        let mut settings = SETTINGS.lock_or_recover();
        if settings.show_seventv_name_paints == show {
            return;
        }
        settings.show_seventv_name_paints = show;
    }
    save_settings();
    bump_layout_settings_version();
    broadcast_settings_event(SettingsEvent::CosmeticSettingsChanged);
}

pub fn get_show_seventv_paint_shadows() -> bool {
    SETTINGS.lock_or_recover().show_seventv_paint_shadows
}

pub fn set_show_seventv_paint_shadows(show: bool) {
    {
        let mut settings = SETTINGS.lock_or_recover();
        if settings.show_seventv_paint_shadows == show {
            return;
        }
        settings.show_seventv_paint_shadows = show;
    }
    save_settings();
    bump_layout_settings_version();
    broadcast_settings_event(SettingsEvent::CosmeticSettingsChanged);
}

pub fn get_show_seventv_cosmetic_badges() -> bool {
    SETTINGS.lock_or_recover().show_seventv_cosmetic_badges
}

pub fn set_show_seventv_cosmetic_badges(show: bool) {
    {
        let mut settings = SETTINGS.lock_or_recover();
        if settings.show_seventv_cosmetic_badges == show {
            return;
        }
        settings.show_seventv_cosmetic_badges = show;
    }
    save_settings();
    bump_layout_settings_version();
    broadcast_settings_event(SettingsEvent::CosmeticSettingsChanged);
}

pub fn get_show_seventv_profile_avatars() -> bool {
    SETTINGS.lock_or_recover().show_seventv_profile_avatars
}

pub fn set_show_seventv_profile_avatars(show: bool) {
    {
        let mut settings = SETTINGS.lock_or_recover();
        if settings.show_seventv_profile_avatars == show {
            return;
        }
        settings.show_seventv_profile_avatars = show;
    }
    save_settings();
    broadcast_settings_event(SettingsEvent::CosmeticSettingsChanged);
}

/// Check if always-on-top window mode is enabled
pub fn get_always_on_top() -> bool {
    SETTINGS.lock_or_recover().always_on_top
}

/// Set always-on-top window mode
pub fn set_always_on_top(enabled: bool) {
    SETTINGS.lock_or_recover().always_on_top = enabled;
    save_settings();
    broadcast_settings_event(SettingsEvent::WindowAppearanceChanged);
}

/// Check if window transparency mode is enabled
pub fn get_window_transparent() -> bool {
    SETTINGS.lock_or_recover().window_transparent
}

/// Fast-path cached window transparency flag for render-time callers.
pub fn get_window_transparent_cached() -> bool {
    CACHED_WINDOW_TRANSPARENT.load(Ordering::Acquire)
}

/// Whether minimizing the main window should hide it in the Windows notification area.
///
/// The native tray owner polls this without taking the settings mutex.
pub fn get_minimize_to_tray() -> bool {
    CACHED_MINIMIZE_TO_TRAY.load(Ordering::Acquire)
}

pub fn set_minimize_to_tray(enabled: bool) {
    {
        let mut settings = SETTINGS.lock_or_recover();
        if settings.minimize_to_tray == enabled {
            return;
        }
        settings.minimize_to_tray = enabled;
    }
    CACHED_MINIMIZE_TO_TRAY.store(enabled, Ordering::Release);
    save_settings();
}

/// Whether closing the main window should hide it in the Windows notification area.
///
/// The native tray owner polls this without taking the settings mutex.
pub fn get_close_to_tray() -> bool {
    CACHED_CLOSE_TO_TRAY.load(Ordering::Acquire)
}

pub fn set_close_to_tray(enabled: bool) {
    {
        let mut settings = SETTINGS.lock_or_recover();
        if settings.close_to_tray == enabled {
            return;
        }
        settings.close_to_tray = enabled;
    }
    CACHED_CLOSE_TO_TRAY.store(enabled, Ordering::Release);
    save_settings();
}

/// Set window transparency mode
pub fn set_window_transparent(enabled: bool) {
    {
        let mut settings = SETTINGS.lock_or_recover();
        if settings.window_transparent == enabled {
            return;
        }
        settings.window_transparent = enabled;
    }
    CACHED_WINDOW_TRANSPARENT.store(enabled, Ordering::Release);
    save_settings();
    broadcast_settings_event(SettingsEvent::WindowAppearanceChanged);
}

pub fn get_overlay_click_through() -> bool {
    SETTINGS.lock_or_recover().overlay_click_through
}

pub fn set_overlay_click_through(enabled: bool) {
    SETTINGS.lock_or_recover().overlay_click_through = enabled;
    save_settings();
    broadcast_settings_event(SettingsEvent::OverlaySettingsChanged);
}

pub fn get_overlay_ghost_mode() -> bool {
    SETTINGS.lock_or_recover().overlay_ghost_mode
}

pub fn set_overlay_ghost_mode(enabled: bool) {
    SETTINGS.lock_or_recover().overlay_ghost_mode = enabled;
    save_settings();
    broadcast_settings_event(SettingsEvent::OverlaySettingsChanged);
}

pub fn get_overlay_window_size() -> (f32, f32) {
    let settings = SETTINGS.lock_or_recover();
    (
        settings.overlay_window_width.clamp(200, 3840) as f32,
        settings.overlay_window_height.clamp(150, 2160) as f32,
    )
}

pub fn set_overlay_window_size(width: f32, height: f32) {
    if !width.is_finite() || !height.is_finite() {
        return;
    }

    let width = (width.round() as u32).clamp(200, 3840);
    let height = (height.round() as u32).clamp(150, 2160);
    let changed = {
        let mut settings = SETTINGS.lock_or_recover();
        if settings.overlay_window_width == width && settings.overlay_window_height == height {
            false
        } else {
            settings.overlay_window_width = width;
            settings.overlay_window_height = height;
            true
        }
    };

    if changed {
        save_settings_debounced();
    }
}

pub fn get_overlay_window_position() -> Option<(f32, f32)> {
    let settings = SETTINGS.lock_or_recover();
    if !settings.overlay_window_has_position {
        return None;
    }

    Some((
        settings.overlay_window_x.clamp(-100_000, 100_000) as f32,
        settings.overlay_window_y.clamp(-100_000, 100_000) as f32,
    ))
}

pub fn set_overlay_window_position(x: f32, y: f32) {
    if !x.is_finite() || !y.is_finite() {
        return;
    }

    let x = x.round().clamp(-100_000.0, 100_000.0) as i32;
    let y = y.round().clamp(-100_000.0, 100_000.0) as i32;
    let changed = {
        let mut settings = SETTINGS.lock_or_recover();
        if settings.overlay_window_has_position
            && settings.overlay_window_x == x
            && settings.overlay_window_y == y
        {
            false
        } else {
            settings.overlay_window_has_position = true;
            settings.overlay_window_x = x;
            settings.overlay_window_y = y;
            true
        }
    };

    if changed {
        save_settings_debounced();
    }
}

pub fn clear_overlay_window_position() {
    let changed = {
        let mut settings = SETTINGS.lock_or_recover();
        if !settings.overlay_window_has_position {
            false
        } else {
            settings.overlay_window_has_position = false;
            settings.overlay_window_x = 0;
            settings.overlay_window_y = 0;
            true
        }
    };

    if changed {
        save_settings();
        broadcast_settings_event(SettingsEvent::OverlaySettingsChanged);
    }
}

pub fn get_overlay_max_messages() -> usize {
    SETTINGS
        .lock_or_recover()
        .overlay_max_messages
        .clamp(5, 100) as usize
}

pub fn set_overlay_max_messages(count: usize) {
    let count = count.clamp(5, 100) as u32;
    SETTINGS.lock_or_recover().overlay_max_messages = count;
    save_settings();
    broadcast_settings_event(SettingsEvent::OverlaySettingsChanged);
}

pub fn get_overlay_idle_fade_secs() -> u64 {
    SETTINGS
        .lock_or_recover()
        .overlay_idle_fade_secs
        .clamp(0, 600) as u64
}

pub fn set_overlay_idle_fade_secs(secs: u64) {
    let secs = secs.clamp(0, 600) as u32;
    SETTINGS.lock_or_recover().overlay_idle_fade_secs = secs;
    save_settings();
    broadcast_settings_event(SettingsEvent::OverlaySettingsChanged);
}

pub fn get_overlay_min_opacity() -> f32 {
    let percent = SETTINGS
        .lock_or_recover()
        .overlay_min_opacity_percent
        .clamp(0, 100) as f32;
    percent / 100.0
}

pub fn set_overlay_min_opacity(opacity: f32) {
    let percent = (opacity.clamp(0.0, 1.0) * 100.0).round() as u8;
    SETTINGS.lock_or_recover().overlay_min_opacity_percent = percent;
    save_settings();
    broadcast_settings_event(SettingsEvent::OverlaySettingsChanged);
}

fn normalize_chat_popup_channel(channel: &str) -> String {
    channel.trim().trim_start_matches('#').trim().to_lowercase()
}

fn legacy_chat_popup_window_descriptor(
    settings: &AppSettings,
    channel: String,
) -> ChatPopupWindowDescriptor {
    ChatPopupWindowDescriptor {
        channel,
        width: settings.chat_popup_window_width.clamp(360, 3840),
        height: settings.chat_popup_window_height.clamp(420, 2160),
        has_position: settings.chat_popup_window_has_position,
        x: settings.chat_popup_window_x.clamp(-100_000, 100_000),
        y: settings.chat_popup_window_y.clamp(-100_000, 100_000),
        is_open: false,
        is_maximized: false,
    }
}

fn trim_chat_popup_window_descriptors(descriptors: &mut Vec<ChatPopupWindowDescriptor>) {
    let mut closed_count = descriptors
        .iter()
        .filter(|descriptor| !descriptor.is_open)
        .count();
    while closed_count > MAX_CLOSED_CHAT_POPUP_WINDOW_DESCRIPTORS {
        let Some(eviction_index) = descriptors
            .iter()
            .position(|descriptor| !descriptor.is_open)
        else {
            break;
        };
        descriptors.remove(eviction_index);
        closed_count -= 1;
    }
}

fn get_chat_popup_window_descriptor(channel: &str) -> ChatPopupWindowDescriptor {
    let channel = normalize_chat_popup_channel(channel);
    let settings = SETTINGS.lock_or_recover();
    settings
        .chat_popup_windows
        .iter()
        .find(|descriptor| descriptor.channel == channel)
        .cloned()
        .unwrap_or_else(|| legacy_chat_popup_window_descriptor(&settings, channel))
}

fn update_chat_popup_window_descriptor(
    channel: &str,
    update: impl FnOnce(&mut ChatPopupWindowDescriptor),
) {
    let channel = normalize_chat_popup_channel(channel);
    if channel.is_empty() {
        return;
    }

    let changed = {
        let mut settings = SETTINGS.lock_or_recover();
        let existing_index = settings
            .chat_popup_windows
            .iter()
            .position(|descriptor| descriptor.channel == channel);
        let mut descriptor = existing_index
            .map(|index| settings.chat_popup_windows.remove(index))
            .unwrap_or_else(|| legacy_chat_popup_window_descriptor(&settings, channel));
        let previous = descriptor.clone();

        update(&mut descriptor);
        descriptor.width = descriptor.width.clamp(360, 3840);
        descriptor.height = descriptor.height.clamp(420, 2160);
        descriptor.x = descriptor.x.clamp(-100_000, 100_000);
        descriptor.y = descriptor.y.clamp(-100_000, 100_000);

        if existing_index.is_some() && descriptor == previous {
            settings
                .chat_popup_windows
                .insert(existing_index.unwrap_or_default(), descriptor);
            false
        } else {
            settings.chat_popup_windows.push(descriptor);
            trim_chat_popup_window_descriptors(&mut settings.chat_popup_windows);
            true
        }
    };

    if changed {
        save_settings_debounced();
    }
}

pub fn get_chat_popup_window_size_for_channel(channel: &str) -> (f32, f32) {
    let descriptor = get_chat_popup_window_descriptor(channel);
    (descriptor.width as f32, descriptor.height as f32)
}

pub fn set_chat_popup_window_size_for_channel(channel: &str, width: f32, height: f32) {
    if !width.is_finite() || !height.is_finite() {
        return;
    }

    let width = (width.round() as u32).clamp(360, 3840);
    let height = (height.round() as u32).clamp(420, 2160);
    set_chat_popup_window_size(width as f32, height as f32);
    update_chat_popup_window_descriptor(channel, |descriptor| {
        descriptor.width = width;
        descriptor.height = height;
    });
}

pub fn get_chat_popup_window_position_for_channel(channel: &str) -> Option<(f32, f32)> {
    let descriptor = get_chat_popup_window_descriptor(channel);
    descriptor
        .has_position
        .then_some((descriptor.x as f32, descriptor.y as f32))
}

pub fn set_chat_popup_window_position_for_channel(channel: &str, x: f32, y: f32) {
    if !x.is_finite() || !y.is_finite() {
        return;
    }

    let x = x.round().clamp(-100_000.0, 100_000.0) as i32;
    let y = y.round().clamp(-100_000.0, 100_000.0) as i32;
    set_chat_popup_window_position(x as f32, y as f32);
    update_chat_popup_window_descriptor(channel, |descriptor| {
        descriptor.has_position = true;
        descriptor.x = x;
        descriptor.y = y;
    });
}

pub fn set_chat_popup_window_open(channel: &str, is_open: bool) {
    update_chat_popup_window_descriptor(channel, |descriptor| {
        descriptor.is_open = is_open;
    });
}

/// Return normalized channels whose native popup should exist in this session.
///
/// The descriptor order is least-to-most recently changed, which also gives
/// startup restoration a stable order without another persisted field.
pub fn get_open_chat_popup_channels() -> Vec<String> {
    SETTINGS
        .lock_or_recover()
        .chat_popup_windows
        .iter()
        .filter(|descriptor| descriptor.is_open)
        .map(|descriptor| descriptor.channel.clone())
        .collect()
}

pub fn get_chat_popup_window_maximized_for_channel(channel: &str) -> bool {
    get_chat_popup_window_descriptor(channel).is_maximized
}

pub fn set_chat_popup_window_maximized_for_channel(channel: &str, is_maximized: bool) {
    update_chat_popup_window_descriptor(channel, |descriptor| {
        descriptor.is_maximized = is_maximized;
    });
}

pub fn set_chat_popup_window_size(width: f32, height: f32) {
    if !width.is_finite() || !height.is_finite() {
        return;
    }

    let width = (width.round() as u32).clamp(360, 3840);
    let height = (height.round() as u32).clamp(420, 2160);
    let changed = {
        let mut settings = SETTINGS.lock_or_recover();
        if settings.chat_popup_window_width == width && settings.chat_popup_window_height == height
        {
            false
        } else {
            settings.chat_popup_window_width = width;
            settings.chat_popup_window_height = height;
            true
        }
    };

    if changed {
        save_settings_debounced();
    }
}

pub fn set_chat_popup_window_position(x: f32, y: f32) {
    if !x.is_finite() || !y.is_finite() {
        return;
    }

    let x = x.round().clamp(-100_000.0, 100_000.0) as i32;
    let y = y.round().clamp(-100_000.0, 100_000.0) as i32;
    let changed = {
        let mut settings = SETTINGS.lock_or_recover();
        if settings.chat_popup_window_has_position
            && settings.chat_popup_window_x == x
            && settings.chat_popup_window_y == y
        {
            false
        } else {
            settings.chat_popup_window_has_position = true;
            settings.chat_popup_window_x = x;
            settings.chat_popup_window_y = y;
            true
        }
    };

    if changed {
        save_settings_debounced();
    }
}

fn default_user_card_popout_window_width() -> u32 {
    470
}

fn default_user_card_popout_window_height() -> u32 {
    560
}

pub fn get_user_card_popout_window_size() -> (f32, f32) {
    let settings = SETTINGS.lock_or_recover();
    (
        settings.user_card_popout_window_width.clamp(320, 3840) as f32,
        settings.user_card_popout_window_height.clamp(420, 2160) as f32,
    )
}

pub fn set_user_card_popout_window_size(width: f32, height: f32) {
    if !width.is_finite() || !height.is_finite() {
        return;
    }

    let width = (width.round() as u32).clamp(320, 3840);
    let height = (height.round() as u32).clamp(420, 2160);
    let changed = {
        let mut settings = SETTINGS.lock_or_recover();
        if settings.user_card_popout_window_width == width
            && settings.user_card_popout_window_height == height
        {
            false
        } else {
            settings.user_card_popout_window_width = width;
            settings.user_card_popout_window_height = height;
            true
        }
    };

    if changed {
        save_settings_debounced();
    }
}

pub fn get_user_card_popout_window_position() -> Option<(f32, f32)> {
    let settings = SETTINGS.lock_or_recover();
    if !settings.user_card_popout_window_has_position {
        return None;
    }

    Some((
        settings.user_card_popout_window_x.clamp(-100_000, 100_000) as f32,
        settings.user_card_popout_window_y.clamp(-100_000, 100_000) as f32,
    ))
}

pub fn set_user_card_popout_window_position(x: f32, y: f32) {
    if !x.is_finite() || !y.is_finite() {
        return;
    }

    let x = x.round().clamp(-100_000.0, 100_000.0) as i32;
    let y = y.round().clamp(-100_000.0, 100_000.0) as i32;
    let changed = {
        let mut settings = SETTINGS.lock_or_recover();
        if settings.user_card_popout_window_has_position
            && settings.user_card_popout_window_x == x
            && settings.user_card_popout_window_y == y
        {
            false
        } else {
            settings.user_card_popout_window_has_position = true;
            settings.user_card_popout_window_x = x;
            settings.user_card_popout_window_y = y;
            true
        }
    };

    if changed {
        save_settings_debounced();
    }
}

pub fn get_emote_picker_popout_window_size() -> (f32, f32) {
    let settings = SETTINGS.lock_or_recover();
    (
        settings.emote_picker_popout_window_width.clamp(320, 3840) as f32,
        settings.emote_picker_popout_window_height.clamp(420, 2160) as f32,
    )
}

pub fn set_emote_picker_popout_window_size(width: f32, height: f32) {
    if !width.is_finite() || !height.is_finite() {
        return;
    }

    let width = (width.round() as u32).clamp(320, 3840);
    let height = (height.round() as u32).clamp(420, 2160);
    let changed = {
        let mut settings = SETTINGS.lock_or_recover();
        if settings.emote_picker_popout_window_width == width
            && settings.emote_picker_popout_window_height == height
        {
            false
        } else {
            settings.emote_picker_popout_window_width = width;
            settings.emote_picker_popout_window_height = height;
            true
        }
    };

    if changed {
        save_settings_debounced();
    }
}

pub fn get_emote_picker_popout_window_position() -> Option<(f32, f32)> {
    let settings = SETTINGS.lock_or_recover();
    if !settings.emote_picker_popout_window_has_position {
        return None;
    }

    Some((
        settings
            .emote_picker_popout_window_x
            .clamp(-100_000, 100_000) as f32,
        settings
            .emote_picker_popout_window_y
            .clamp(-100_000, 100_000) as f32,
    ))
}

/// A window's size and, when known, its position.
pub type WindowGeometry = ((f32, f32), Option<(f32, f32)>);

pub fn get_emote_picker_popout_window_geometry_for_display(
    display_uuid: &str,
) -> Option<WindowGeometry> {
    let settings = SETTINGS.lock_or_recover();
    let geometry = settings
        .emote_picker_popout_display_geometries
        .iter()
        .find(|geometry| geometry.display_uuid == display_uuid)?;
    let size = (
        geometry.width.clamp(320, 3840) as f32,
        geometry.height.clamp(420, 2160) as f32,
    );
    let position = geometry.has_position.then(|| {
        (
            geometry.x.clamp(-100_000, 100_000) as f32,
            geometry.y.clamp(-100_000, 100_000) as f32,
        )
    });
    Some((size, position))
}

pub fn set_emote_picker_popout_window_position(x: f32, y: f32) {
    if !x.is_finite() || !y.is_finite() {
        return;
    }

    let x = x.round().clamp(-100_000.0, 100_000.0) as i32;
    let y = y.round().clamp(-100_000.0, 100_000.0) as i32;
    let changed = {
        let mut settings = SETTINGS.lock_or_recover();
        if settings.emote_picker_popout_window_has_position
            && settings.emote_picker_popout_window_x == x
            && settings.emote_picker_popout_window_y == y
        {
            false
        } else {
            settings.emote_picker_popout_window_has_position = true;
            settings.emote_picker_popout_window_x = x;
            settings.emote_picker_popout_window_y = y;
            true
        }
    };

    if changed {
        save_settings_debounced();
    }
}

pub fn set_emote_picker_popout_window_geometry_for_display(
    display_uuid: &str,
    x: f32,
    y: f32,
    width: f32,
    height: f32,
) {
    if display_uuid.is_empty()
        || display_uuid.len() > 128
        || !x.is_finite()
        || !y.is_finite()
        || !width.is_finite()
        || !height.is_finite()
    {
        return;
    }

    let x = x.round().clamp(-100_000.0, 100_000.0) as i32;
    let y = y.round().clamp(-100_000.0, 100_000.0) as i32;
    let width = (width.round() as u32).clamp(320, 3840);
    let height = (height.round() as u32).clamp(420, 2160);
    let changed = {
        let mut settings = SETTINGS.lock_or_recover();
        let legacy_changed = !settings.emote_picker_popout_window_has_position
            || settings.emote_picker_popout_window_x != x
            || settings.emote_picker_popout_window_y != y
            || settings.emote_picker_popout_window_width != width
            || settings.emote_picker_popout_window_height != height;
        settings.emote_picker_popout_window_has_position = true;
        settings.emote_picker_popout_window_x = x;
        settings.emote_picker_popout_window_y = y;
        settings.emote_picker_popout_window_width = width;
        settings.emote_picker_popout_window_height = height;

        let geometry_changed = match settings
            .emote_picker_popout_display_geometries
            .iter_mut()
            .find(|geometry| geometry.display_uuid == display_uuid)
        {
            Some(geometry) => {
                let changed = geometry.width != width
                    || geometry.height != height
                    || !geometry.has_position
                    || geometry.x != x
                    || geometry.y != y;
                geometry.width = width;
                geometry.height = height;
                geometry.has_position = true;
                geometry.x = x;
                geometry.y = y;
                changed
            }
            None => {
                settings.emote_picker_popout_display_geometries.push(
                    EmotePickerPopoutDisplayGeometry {
                        display_uuid: display_uuid.to_string(),
                        width,
                        height,
                        has_position: true,
                        x,
                        y,
                    },
                );
                true
            }
        };
        legacy_changed || geometry_changed
    };

    if changed {
        save_settings_debounced();
    }
}

pub fn get_settings_popout_window_size() -> (f32, f32) {
    let settings = SETTINGS.lock_or_recover();
    (
        settings.settings_popout_window_width.clamp(600, 3840) as f32,
        settings.settings_popout_window_height.clamp(400, 2160) as f32,
    )
}

pub fn set_settings_popout_window_size(width: f32, height: f32) {
    if !width.is_finite() || !height.is_finite() {
        return;
    }

    let width = (width.round() as u32).clamp(600, 3840);
    let height = (height.round() as u32).clamp(400, 2160);
    let changed = {
        let mut settings = SETTINGS.lock_or_recover();
        if settings.settings_popout_window_width == width
            && settings.settings_popout_window_height == height
        {
            false
        } else {
            settings.settings_popout_window_width = width;
            settings.settings_popout_window_height = height;
            true
        }
    };

    if changed {
        save_settings_debounced();
    }
}

pub fn get_settings_popout_window_position() -> Option<(f32, f32)> {
    let settings = SETTINGS.lock_or_recover();
    if !settings.settings_popout_window_has_position {
        return None;
    }

    Some((
        settings.settings_popout_window_x.clamp(-100_000, 100_000) as f32,
        settings.settings_popout_window_y.clamp(-100_000, 100_000) as f32,
    ))
}

pub fn set_settings_popout_window_position(x: f32, y: f32) {
    if !x.is_finite() || !y.is_finite() {
        return;
    }

    let x = x.round().clamp(-100_000.0, 100_000.0) as i32;
    let y = y.round().clamp(-100_000.0, 100_000.0) as i32;
    let changed = {
        let mut settings = SETTINGS.lock_or_recover();
        if settings.settings_popout_window_has_position
            && settings.settings_popout_window_x == x
            && settings.settings_popout_window_y == y
        {
            false
        } else {
            settings.settings_popout_window_has_position = true;
            settings.settings_popout_window_x = x;
            settings.settings_popout_window_y = y;
            true
        }
    };

    if changed {
        save_settings_debounced();
    }
}

pub fn get_debug_popup_window_size() -> (f32, f32) {
    let settings = SETTINGS.lock_or_recover();
    (
        settings.debug_popup_window_width.clamp(520, 3840) as f32,
        settings.debug_popup_window_height.clamp(520, 2160) as f32,
    )
}

pub fn set_debug_popup_window_size(width: f32, height: f32) {
    if !width.is_finite() || !height.is_finite() {
        return;
    }

    let width = (width.round() as u32).clamp(520, 3840);
    let height = (height.round() as u32).clamp(520, 2160);
    let changed = {
        let mut settings = SETTINGS.lock_or_recover();
        if settings.debug_popup_window_width == width
            && settings.debug_popup_window_height == height
        {
            false
        } else {
            settings.debug_popup_window_width = width;
            settings.debug_popup_window_height = height;
            true
        }
    };

    if changed {
        save_settings_debounced();
    }
}

pub fn get_debug_popup_window_position() -> Option<(f32, f32)> {
    let settings = SETTINGS.lock_or_recover();
    if !settings.debug_popup_window_has_position {
        return None;
    }

    Some((
        settings.debug_popup_window_x.clamp(-100_000, 100_000) as f32,
        settings.debug_popup_window_y.clamp(-100_000, 100_000) as f32,
    ))
}

pub fn set_debug_popup_window_position(x: f32, y: f32) {
    if !x.is_finite() || !y.is_finite() {
        return;
    }

    let x = x.round().clamp(-100_000.0, 100_000.0) as i32;
    let y = y.round().clamp(-100_000.0, 100_000.0) as i32;
    let changed = {
        let mut settings = SETTINGS.lock_or_recover();
        if settings.debug_popup_window_has_position
            && settings.debug_popup_window_x == x
            && settings.debug_popup_window_y == y
        {
            false
        } else {
            settings.debug_popup_window_has_position = true;
            settings.debug_popup_window_x = x;
            settings.debug_popup_window_y = y;
            true
        }
    };

    if changed {
        save_settings_debounced();
    }
}

/// Native main-window minimums. Keep this contract shared by settings and
/// startup so a user-resized window is not enlarged again at the next launch.
pub const MAIN_WINDOW_MIN_WIDTH_PX: u32 = 300;
pub const MAIN_WINDOW_MIN_HEIGHT_PX: u32 = 400;

/// Get persisted main window size, clamped to sane visible bounds.
pub fn get_main_window_size() -> (f32, f32) {
    let settings = SETTINGS.lock_or_recover();
    (
        settings
            .main_window_width
            .clamp(MAIN_WINDOW_MIN_WIDTH_PX, 7680) as f32,
        settings
            .main_window_height
            .clamp(MAIN_WINDOW_MIN_HEIGHT_PX, 4320) as f32,
    )
}

/// Persist the main window size using the debounced settings path.
pub fn set_main_window_size(width: f32, height: f32) {
    if !width.is_finite() || !height.is_finite() {
        return;
    }

    let width = (width.round() as u32).clamp(MAIN_WINDOW_MIN_WIDTH_PX, 7680);
    let height = (height.round() as u32).clamp(MAIN_WINDOW_MIN_HEIGHT_PX, 4320);
    let changed = {
        let mut settings = SETTINGS.lock_or_recover();
        if settings.main_window_width == width && settings.main_window_height == height {
            false
        } else {
            settings.main_window_width = width;
            settings.main_window_height = height;
            true
        }
    };

    if changed {
        save_settings_debounced();
    }
}

/// Get whether the main window should restore maximized.
pub fn get_main_window_maximized() -> bool {
    SETTINGS.lock_or_recover().main_window_maximized
}

/// Persist whether the main window is maximized.
pub fn set_main_window_maximized(enabled: bool) {
    let changed = {
        let mut settings = SETTINGS.lock_or_recover();
        if settings.main_window_maximized == enabled {
            false
        } else {
            settings.main_window_maximized = enabled;
            true
        }
    };

    if changed {
        save_settings_debounced();
    }
}

/// Get persisted main window position when one has been observed.
pub fn get_main_window_position() -> Option<(f32, f32)> {
    let settings = SETTINGS.lock_or_recover();
    if !settings.main_window_has_position {
        return None;
    }

    Some((
        settings.main_window_x.clamp(-100_000, 100_000) as f32,
        settings.main_window_y.clamp(-100_000, 100_000) as f32,
    ))
}

/// Persist the main window position using the debounced settings path.
pub fn set_main_window_position(x: f32, y: f32) {
    if !x.is_finite() || !y.is_finite() {
        return;
    }

    let x = x.round().clamp(-100_000.0, 100_000.0) as i32;
    let y = y.round().clamp(-100_000.0, 100_000.0) as i32;
    let changed = {
        let mut settings = SETTINGS.lock_or_recover();
        if settings.main_window_has_position
            && settings.main_window_x == x
            && settings.main_window_y == y
        {
            false
        } else {
            settings.main_window_has_position = true;
            settings.main_window_x = x;
            settings.main_window_y = y;
            true
        }
    };

    if changed {
        save_settings_debounced();
    }
}

/// Modifier that pauses bottom-follow while held. This is an atomic read on the
/// message/render hot path.
pub fn get_chat_pause_modifier() -> ChatPauseModifier {
    ChatPauseModifier::from_u8(CACHED_CHAT_PAUSE_MODIFIER.load(Ordering::Acquire))
}

pub fn set_chat_pause_modifier(modifier: ChatPauseModifier) {
    {
        let mut settings = SETTINGS.lock_or_recover();
        if settings.chat_pause_modifier == modifier {
            return;
        }
        settings.chat_pause_modifier = modifier;
    }
    CACHED_CHAT_PAUSE_MODIFIER.store(modifier as u8, Ordering::Release);
    save_settings();
    broadcast_settings_event(SettingsEvent::ChatPauseChanged);
}

/// Optional mouse hover pause mode. Disabled is the default and keeps ordinary
/// pointer movement behavior-neutral.
pub fn get_chat_hover_pause_mode() -> ChatHoverPauseMode {
    ChatHoverPauseMode::from_u8(CACHED_CHAT_HOVER_PAUSE_MODE.load(Ordering::Acquire))
}

pub fn set_chat_hover_pause_mode(mode: ChatHoverPauseMode) {
    {
        let mut settings = SETTINGS.lock_or_recover();
        if settings.chat_hover_pause_mode == mode {
            return;
        }
        settings.chat_hover_pause_mode = mode;
    }
    CACHED_CHAT_HOVER_PAUSE_MODE.store(mode as u8, Ordering::Release);
    save_settings();
    broadcast_settings_event(SettingsEvent::ChatPauseChanged);
}

pub fn get_username_right_click_behavior() -> UsernameRightClickBehavior {
    UsernameRightClickBehavior::from_u8(
        CACHED_USERNAME_RIGHT_CLICK_BEHAVIOR.load(Ordering::Acquire),
    )
}

pub fn set_username_right_click_behavior(behavior: UsernameRightClickBehavior) {
    {
        let mut settings = SETTINGS.lock_or_recover();
        if settings.username_right_click_behavior == behavior {
            return;
        }
        settings.username_right_click_behavior = behavior;
    }
    CACHED_USERNAME_RIGHT_CLICK_BEHAVIOR.store(behavior as u8, Ordering::Release);
    save_settings();
    broadcast_settings_event(SettingsEvent::ChatInteractionChanged);
}

pub fn get_username_right_click_modifier_behavior() -> UsernameRightClickBehavior {
    UsernameRightClickBehavior::from_u8(
        CACHED_USERNAME_RIGHT_CLICK_MODIFIER_BEHAVIOR.load(Ordering::Acquire),
    )
}

pub fn set_username_right_click_modifier_behavior(behavior: UsernameRightClickBehavior) {
    {
        let mut settings = SETTINGS.lock_or_recover();
        if settings.username_right_click_modifier_behavior == behavior {
            return;
        }
        settings.username_right_click_modifier_behavior = behavior;
    }
    CACHED_USERNAME_RIGHT_CLICK_MODIFIER_BEHAVIOR.store(behavior as u8, Ordering::Release);
    save_settings();
    broadcast_settings_event(SettingsEvent::ChatInteractionChanged);
}

pub fn get_username_right_click_modifier() -> UsernameRightClickModifier {
    UsernameRightClickModifier::from_u8(
        CACHED_USERNAME_RIGHT_CLICK_MODIFIER.load(Ordering::Acquire),
    )
}

pub fn set_username_right_click_modifier(modifier: UsernameRightClickModifier) {
    {
        let mut settings = SETTINGS.lock_or_recover();
        if settings.username_right_click_modifier == modifier {
            return;
        }
        settings.username_right_click_modifier = modifier;
    }
    CACHED_USERNAME_RIGHT_CLICK_MODIFIER.store(modifier as u8, Ordering::Release);
    save_settings();
    broadcast_settings_event(SettingsEvent::ChatInteractionChanged);
}

pub fn get_show_reply_button() -> bool {
    CACHED_SHOW_REPLY_BUTTON.load(Ordering::Acquire)
}

pub fn set_show_reply_button(show: bool) {
    {
        let mut settings = SETTINGS.lock_or_recover();
        if settings.show_reply_button == show {
            return;
        }
        settings.show_reply_button = show;
    }
    CACHED_SHOW_REPLY_BUTTON.store(show, Ordering::Release);
    save_settings();
    broadcast_settings_event(SettingsEvent::ChatInteractionChanged);
}

pub fn get_show_last_read_indicator() -> bool {
    CACHED_SHOW_LAST_READ_INDICATOR.load(Ordering::Acquire)
}

pub fn set_show_last_read_indicator(show: bool) {
    {
        let mut settings = SETTINGS.lock_or_recover();
        if settings.show_last_read_indicator == show {
            return;
        }
        settings.show_last_read_indicator = show;
    }
    CACHED_SHOW_LAST_READ_INDICATOR.store(show, Ordering::Release);
    save_settings_debounced();
    broadcast_settings_event(SettingsEvent::ChatInteractionChanged);
}

/// Check if chat pause indicator is shown
pub fn get_show_pause_indicator() -> bool {
    CACHED_SHOW_PAUSE_INDICATOR.load(Ordering::Acquire)
}

/// Set chat pause indicator visibility
pub fn set_show_pause_indicator(show: bool) {
    SETTINGS.lock_or_recover().show_pause_indicator = show;
    CACHED_SHOW_PAUSE_INDICATOR.store(show, Ordering::Release);
    save_settings();
}

pub const MOUSE_SCROLL_MULTIPLIER_MIN: f32 = 0.1;
pub const MOUSE_SCROLL_MULTIPLIER_MAX: f32 = 5.0;

fn clamp_mouse_scroll_multiplier(multiplier: f32) -> f32 {
    if multiplier.is_finite() {
        multiplier.clamp(MOUSE_SCROLL_MULTIPLIER_MIN, MOUSE_SCROLL_MULTIPLIER_MAX)
    } else {
        1.0
    }
}

/// Multiplier applied to mouse-wheel line deltas; 1.0 keeps the default speed.
pub fn get_mouse_scroll_multiplier() -> f32 {
    f32::from_bits(CACHED_MOUSE_SCROLL_MULTIPLIER.load(Ordering::Acquire))
}

pub fn set_mouse_scroll_multiplier(multiplier: f32) {
    let multiplier = clamp_mouse_scroll_multiplier(multiplier);
    {
        let mut settings = SETTINGS.lock_or_recover();
        if (settings.mouse_scroll_multiplier - multiplier).abs() < 0.0001 {
            return;
        }
        settings.mouse_scroll_multiplier = multiplier;
        CACHED_MOUSE_SCROLL_MULTIPLIER.store(multiplier.to_bits(), Ordering::Release);
    }
    save_settings_debounced();
}

/// Check if character counter is shown
pub fn get_show_char_counter() -> bool {
    SETTINGS.lock_or_recover().show_char_counter
}

/// Set character counter visibility
pub fn set_show_char_counter(show: bool) {
    SETTINGS.lock_or_recover().show_char_counter = show;
    save_settings();
}

/// Whether every pasted image asks for confirmation before uploading.
pub fn get_ask_before_paste_upload() -> bool {
    SETTINGS.lock_or_recover().ask_before_paste_upload
}

pub fn set_ask_before_paste_upload(ask: bool) {
    {
        let mut settings = SETTINGS.lock_or_recover();
        if settings.ask_before_paste_upload == ask {
            return;
        }
        settings.ask_before_paste_upload = ask;
    }
    save_settings_debounced();
}

/// Whether message timestamps are enabled globally.
#[inline]
pub fn get_show_message_timestamps() -> bool {
    CACHED_SHOW_MESSAGE_TIMESTAMPS.load(Ordering::Acquire)
}

/// Closest legacy-enum projection of the active token pattern.
///
/// Retained for surfaces that still store a `MessageTimestampFormat` (derived
/// row caches, legacy previews). It is lossy by construction — custom
/// separators and token order collapse away — so anything that renders a
/// timestamp should use [`message_timestamp_formatter`] instead. Cache keys
/// that use it must also fold in `get_layout_settings_version`, which changes
/// on every pattern edit.
pub fn get_message_timestamp_format() -> MessageTimestampFormat {
    let formatter = message_timestamp_formatter();
    let mut twelve_hour = false;
    let mut padded_hour = false;
    for op in formatter.ops.iter() {
        match op {
            TimestampFormatOp::Hour12 { padded } => {
                twelve_hour = true;
                padded_hour = *padded;
            }
            TimestampFormatOp::Hour24 { padded } => padded_hour = *padded,
            _ => {}
        }
    }
    let precision = match formatter.resolution {
        TimestampResolution::Minutes => MessageTimestampPrecision::Minutes,
        TimestampResolution::Seconds => MessageTimestampPrecision::Seconds,
        TimestampResolution::Milliseconds => MessageTimestampPrecision::Milliseconds,
    };
    MessageTimestampFormat::from_parts(twelve_hour, padded_hour, precision)
}

/// Resolve timestamp visibility for a channel's current live state.
///
/// Both policy bits are atomic mirrors: this runs per split per frame and must
/// not take the global settings mutex.
#[inline]
pub fn should_show_message_timestamps(channel_is_live: bool) -> bool {
    get_show_message_timestamps()
        && !(channel_is_live && CACHED_HIDE_MESSAGE_TIMESTAMPS_WHEN_LIVE.load(Ordering::Acquire))
}

pub fn set_show_message_timestamps(show: bool) {
    {
        let mut settings = SETTINGS.lock_or_recover();
        if settings.show_message_timestamps == show {
            return;
        }
        settings.show_message_timestamps = show;
    }
    CACHED_SHOW_MESSAGE_TIMESTAMPS.store(show, Ordering::Release);
    save_settings();
    bump_layout_settings_version();
    broadcast_settings_event(SettingsEvent::TimestampChanged);
}

/// Current user-authored timestamp pattern.
pub fn get_message_timestamp_pattern() -> TimestampPattern {
    SETTINGS
        .lock_or_recover()
        .message_timestamp_pattern
        .clone()
        .unwrap_or_default()
}

/// Persist a new timestamp pattern and republish the compiled formatter.
///
/// Rejects invalid patterns instead of storing them, so the compiled formatter
/// and every derived cache can assume validity.
pub fn set_message_timestamp_pattern(
    pattern: TimestampPattern,
) -> Result<(), TimestampPatternError> {
    pattern.validate()?;
    {
        let mut settings = SETTINGS.lock_or_recover();
        if settings.message_timestamp_pattern.as_ref() == Some(&pattern) {
            return Ok(());
        }
        settings.message_timestamp_pattern = Some(pattern.clone());
        settings.message_timestamp_format = None;
    }
    publish_compiled_timestamp_formatter(&pattern);
    save_settings();
    bump_layout_settings_version();
    broadcast_settings_event(SettingsEvent::TimestampChanged);
    Ok(())
}

pub fn get_hide_message_timestamps_when_live() -> bool {
    CACHED_HIDE_MESSAGE_TIMESTAMPS_WHEN_LIVE.load(Ordering::Acquire)
}

pub fn set_hide_message_timestamps_when_live(hide: bool) {
    {
        let mut settings = SETTINGS.lock_or_recover();
        if settings.hide_message_timestamps_when_live == hide {
            return;
        }
        settings.hide_message_timestamps_when_live = hide;
    }
    CACHED_HIDE_MESSAGE_TIMESTAMPS_WHEN_LIVE.store(hide, Ordering::Release);
    save_settings();
    bump_layout_settings_version();
    broadcast_settings_event(SettingsEvent::TimestampChanged);
}

/// Check if ASCII art collapse is enabled
pub fn get_collapse_ascii_art() -> bool {
    SETTINGS.lock_or_recover().collapse_ascii_art
}

/// Set ASCII art collapse setting
pub fn set_collapse_ascii_art(enabled: bool) {
    {
        let mut settings = SETTINGS.lock_or_recover();
        if settings.collapse_ascii_art == enabled {
            return;
        }
        settings.collapse_ascii_art = enabled;
    }
    save_settings();
    bump_layout_settings_version();
}

/// Check if pronouns should be shown in user cards
pub fn get_show_pronouns() -> bool {
    SETTINGS.lock_or_recover().show_pronouns
}

/// Set whether to show pronouns in user cards
pub fn set_show_pronouns(enabled: bool) {
    SETTINGS.lock_or_recover().show_pronouns = enabled;
    save_settings();
}

/// Check if follow date should be shown in user cards
pub fn get_show_follow_date() -> bool {
    SETTINGS.lock_or_recover().show_follow_date
}

/// Set whether to show follow date in user cards
pub fn set_show_follow_date(enabled: bool) {
    SETTINGS.lock_or_recover().show_follow_date = enabled;
    save_settings();
}

/// Check if follower count should be shown in user cards
pub fn get_show_follower_count() -> bool {
    SETTINGS.lock_or_recover().show_follower_count
}

/// Set whether to show follower count in user cards
pub fn set_show_follower_count(enabled: bool) {
    SETTINGS.lock_or_recover().show_follower_count = enabled;
    save_settings();
}

/// Get watchstreak highlight color
pub fn get_watchstreak_highlight_color() -> String {
    SETTINGS
        .lock_or_recover()
        .watchstreak_highlight_color
        .clone()
}

/// Set watchstreak highlight color
pub fn set_watchstreak_highlight_color(color: String) {
    SETTINGS.lock_or_recover().watchstreak_highlight_color = color;
    save_settings_debounced();
    broadcast_settings_event(SettingsEvent::HighlightColorChanged);
}

/// Get automod highlight color
pub fn get_automod_highlight_color() -> String {
    SETTINGS.lock_or_recover().automod_highlight_color.clone()
}

/// Set automod highlight color
pub fn set_automod_highlight_color(color: String) {
    SETTINGS.lock_or_recover().automod_highlight_color = color;
    save_settings_debounced();
    broadcast_settings_event(SettingsEvent::HighlightColorChanged);
}

pub fn get_announcement_highlight_color() -> String {
    SETTINGS
        .lock_or_recover()
        .announcement_highlight_color
        .clone()
}

pub fn set_announcement_highlight_color(color: String) {
    SETTINGS.lock_or_recover().announcement_highlight_color = color;
    save_settings_debounced();
    broadcast_settings_event(SettingsEvent::HighlightColorChanged);
}

pub fn get_raid_highlight_color() -> String {
    SETTINGS.lock_or_recover().raid_highlight_color.clone()
}

pub fn set_raid_highlight_color(color: String) {
    SETTINGS.lock_or_recover().raid_highlight_color = color;
    save_settings_debounced();
    broadcast_settings_event(SettingsEvent::HighlightColorChanged);
}

pub fn get_bits_highlight_color() -> String {
    SETTINGS.lock_or_recover().bits_highlight_color.clone()
}

pub fn set_bits_highlight_color(color: String) {
    SETTINGS.lock_or_recover().bits_highlight_color = color;
    save_settings_debounced();
    broadcast_settings_event(SettingsEvent::HighlightColorChanged);
}

pub fn get_phrase_highlight_color() -> String {
    SETTINGS.lock_or_recover().phrase_highlight_color.clone()
}

pub fn set_phrase_highlight_color(color: String) {
    SETTINGS.lock_or_recover().phrase_highlight_color = color;
    save_settings_debounced();
    broadcast_settings_event(SettingsEvent::HighlightColorChanged);
}

pub fn get_redeem_highlight_color() -> String {
    SETTINGS.lock_or_recover().redeem_highlight_color.clone()
}

pub fn set_redeem_highlight_color(color: String) {
    SETTINGS.lock_or_recover().redeem_highlight_color = color;
    save_settings_debounced();
    broadcast_settings_event(SettingsEvent::HighlightColorChanged);
}

pub fn get_elevated_highlight_color() -> String {
    SETTINGS.lock_or_recover().elevated_highlight_color.clone()
}

pub fn set_elevated_highlight_color(color: String) {
    SETTINGS.lock_or_recover().elevated_highlight_color = color;
    save_settings_debounced();
    broadcast_settings_event(SettingsEvent::HighlightColorChanged);
}

pub fn get_self_message_highlight_color() -> String {
    SETTINGS
        .lock_or_recover()
        .self_message_highlight_color
        .clone()
}

pub fn set_self_message_highlight_color(color: String) {
    SETTINGS.lock_or_recover().self_message_highlight_color = color;
    save_settings_debounced();
    broadcast_settings_event(SettingsEvent::HighlightColorChanged);
}

pub fn get_enable_self_message_highlight() -> bool {
    SETTINGS.lock_or_recover().enable_self_message_highlight
}

pub fn set_enable_self_message_highlight(enabled: bool) {
    SETTINGS.lock_or_recover().enable_self_message_highlight = enabled;
    save_settings();
    broadcast_settings_event(SettingsEvent::HighlightColorChanged);
}

pub fn get_enable_elevated_highlight() -> bool {
    SETTINGS.lock_or_recover().enable_elevated_highlight
}

pub fn set_enable_elevated_highlight(enabled: bool) {
    SETTINGS.lock_or_recover().enable_elevated_highlight = enabled;
    save_settings();
    broadcast_settings_event(SettingsEvent::HighlightColorChanged);
}

/// Check if manual proxy settings are enabled
pub fn get_proxy_enabled() -> bool {
    SETTINGS.lock_or_recover().proxy_enabled
}

/// Enable or disable manual proxy settings
pub fn set_proxy_enabled(enabled: bool) {
    let changed = {
        let mut settings = SETTINGS.lock_or_recover();
        if settings.proxy_enabled == enabled {
            false
        } else {
            settings.proxy_enabled = enabled;
            true
        }
    };
    if !changed {
        return;
    }
    super::http::mark_proxy_settings_changed();
    save_settings();
    broadcast_settings_event(SettingsEvent::ProxyChanged);
}

/// Get the configured manual proxy URL
pub fn get_proxy_url() -> String {
    SETTINGS.lock_or_recover().proxy_url.clone()
}

/// Set the configured manual proxy URL
pub fn set_proxy_url(url: String) {
    let changed = {
        let mut settings = SETTINGS.lock_or_recover();
        if settings.proxy_url == url {
            false
        } else {
            settings.proxy_url = url;
            true
        }
    };
    if !changed {
        return;
    }
    super::http::mark_proxy_settings_changed();
    save_settings();
    broadcast_settings_event(SettingsEvent::ProxyChanged);
}

/// Check if human-readable channel chat logging is enabled
pub fn get_chat_logging_enabled() -> bool {
    SETTINGS.lock_or_recover().chat_logging_enabled
}

/// Set human-readable channel chat logging
pub fn set_chat_logging_enabled(enabled: bool) {
    SETTINGS.lock_or_recover().chat_logging_enabled = enabled;
    save_settings();
}

/// Check if recent per-channel message history is persisted to disk
pub fn get_persist_history_enabled() -> bool {
    SETTINGS.lock_or_recover().persist_history_enabled
}

/// Set whether recent per-channel message history is persisted to disk
pub fn set_persist_history_enabled(enabled: bool) {
    SETTINGS.lock_or_recover().persist_history_enabled = enabled;
    save_settings();
}

/// Get the optional custom human-readable chat log directory
pub fn get_log_directory() -> Option<String> {
    if crate::benchmark_profile::is_enabled() {
        return None;
    }
    SETTINGS.lock_or_recover().log_directory.clone()
}

/// Set the optional custom human-readable chat log directory
pub fn set_log_directory(path: Option<String>) {
    if crate::benchmark_profile::is_enabled() {
        return;
    }
    let path = normalize_optional_path(path);
    if path.as_deref().is_some_and(is_remote_or_device_path) {
        crate::log_persist!(
            warn,
            "Ignoring log directory: network (UNC) and device paths are not allowed"
        );
        return;
    }
    SETTINGS.lock_or_recover().log_directory = path;
    save_settings();
}

/// Get the configured human-readable chat log file extension/format
pub fn get_log_format() -> String {
    SETTINGS.lock_or_recover().log_format.clone()
}

/// Set the configured human-readable chat log file extension/format
pub fn set_log_format(format: String) {
    SETTINGS.lock_or_recover().log_format = normalize_log_format_value(&format);
    save_settings();
}

/// Check if server timestamps should be logged
pub fn get_log_server_timestamps() -> bool {
    SETTINGS.lock_or_recover().log_server_timestamps
}

/// Set server timestamp logging
pub fn set_log_server_timestamps(enabled: bool) {
    SETTINGS.lock_or_recover().log_server_timestamps = enabled;
    save_settings();
}

/// Check if logging should be organized by stream ID
pub fn get_log_by_stream_id() -> bool {
    SETTINGS.lock_or_recover().log_by_stream_id
}

/// Set logging organization by stream ID
pub fn set_log_by_stream_id(enabled: bool) {
    SETTINGS.lock_or_recover().log_by_stream_id = enabled;
    save_settings();
}

/// Check if deleted messages should be logged
pub fn get_log_deleted_messages() -> bool {
    SETTINGS.lock_or_recover().log_deleted_messages
}

/// Set deleted message logging
pub fn set_log_deleted_messages(enabled: bool) {
    SETTINGS.lock_or_recover().log_deleted_messages = enabled;
    save_settings();
}

/// Get the character limit for deleted message logs
pub fn get_deleted_log_char_limit() -> usize {
    SETTINGS.lock_or_recover().deleted_log_char_limit
}

/// Set the character limit for deleted message logs
pub fn set_deleted_log_char_limit(limit: usize) {
    SETTINGS.lock_or_recover().deleted_log_char_limit =
        normalize_deleted_log_char_limit_value(limit);
    save_settings();
}

/// Check if links should be opened in incognito mode
pub fn get_open_links_incognito() -> bool {
    SETTINGS.lock_or_recover().open_links_incognito
}

/// Set incognito mode for link opening
pub fn set_open_links_incognito(enabled: bool) {
    SETTINGS.lock_or_recover().open_links_incognito = enabled;
    save_settings();
}

/// Whether all valid external HTTP(S) links may open without asking.
pub fn get_skip_open_link_confirmation() -> bool {
    SETTINGS.lock_or_recover().skip_open_link_confirmation
}

/// Enable or disable the global external-link confirmation.
pub fn set_skip_open_link_confirmation(skip: bool) {
    SETTINGS.lock_or_recover().skip_open_link_confirmation = skip;
    save_settings();
}

/// Return the exact hosts that have been explicitly trusted by the user.
pub fn get_trusted_link_domains() -> Vec<String> {
    SETTINGS.lock_or_recover().trusted_link_domains.clone()
}

/// Extract the normalized exact host from an HTTP(S) URL.
pub fn external_link_host(url: &str) -> Option<String> {
    let parsed = url::Url::parse(url).ok()?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return None;
    }
    parsed
        .host_str()
        .map(|host| host.trim_end_matches('.').to_ascii_lowercase())
}

/// Trust one exact host. Subdomains are deliberately independent trust scopes.
pub fn trust_link_domain(host: &str) -> bool {
    let normalized = host.trim().trim_end_matches('.').to_ascii_lowercase();
    if normalized.is_empty()
        || normalized.len() > 253
        || normalized.contains('/')
        || normalized.contains(':')
    {
        return false;
    }

    let changed = {
        let mut settings = SETTINGS.lock_or_recover();
        if settings
            .trusted_link_domains
            .iter()
            .any(|trusted| trusted.eq_ignore_ascii_case(&normalized))
        {
            false
        } else {
            if settings.trusted_link_domains.len() >= 128 {
                settings.trusted_link_domains.remove(0);
            }
            settings.trusted_link_domains.push(normalized);
            true
        }
    };
    if changed {
        save_settings();
    }
    changed
}

/// Forget every per-domain trust decision.
pub fn clear_trusted_link_domains() -> usize {
    let removed = {
        let mut settings = SETTINGS.lock_or_recover();
        let removed = settings.trusted_link_domains.len();
        settings.trusted_link_domains.clear();
        removed
    };
    if removed > 0 {
        save_settings();
    }
    removed
}

/// Check whether a valid HTTP(S) link still requires a confirmation prompt.
pub fn should_confirm_open_link(url: &str) -> bool {
    let Some(host) = external_link_host(url) else {
        return true;
    };
    let settings = SETTINGS.lock_or_recover();
    !settings.skip_open_link_confirmation
        && !settings
            .trusted_link_domains
            .iter()
            .any(|trusted| trusted.eq_ignore_ascii_case(&host))
}

/// Whether Echo should check the signed stable manifest shortly after startup.
pub fn get_auto_check_updates() -> bool {
    SETTINGS.lock_or_recover().auto_check_updates
}

/// Enable or disable automatic update checks. Manual checks remain available.
pub fn set_auto_check_updates(enabled: bool) {
    SETTINGS.lock_or_recover().auto_check_updates = enabled;
    save_settings();
}

/// Get custom browser path
pub fn get_browser_path() -> Option<String> {
    SETTINGS.lock_or_recover().browser_path.clone()
}

/// Set custom browser path
pub fn set_browser_path(path: Option<String>) {
    if let Err(err) = set_browser_path_checked(path) {
        tracing::warn!("Rejected custom browser path: {err}");
    }
}

/// Validate and set the custom browser path.
///
/// Bare command names are accepted so users can provide `chrome` or `msedge`
/// when those commands are on PATH. Values that look like filesystem paths must
/// resolve to an existing file before they are persisted.
pub fn set_browser_path_checked(path: Option<String>) -> Result<Option<String>, String> {
    let normalized = normalize_executable_setting(path, "Browser executable")?;
    SETTINGS.lock_or_recover().browser_path = normalized.clone();
    save_settings();
    Ok(normalized)
}

fn normalize_executable_setting(
    path: Option<String>,
    executable_label: &str,
) -> Result<Option<String>, String> {
    let Some(path) = path else {
        return Ok(None);
    };
    let trimmed = path.trim().trim_matches('"').trim_matches('\'').trim();

    if trimmed.is_empty() {
        return Ok(None);
    }
    if is_remote_or_device_path(trimmed) {
        return Err(format!(
            "{executable_label} path must be on this computer, not a network or device path"
        ));
    }

    if looks_like_executable_file_path(trimmed) {
        let path = PathBuf::from(trimmed);
        let metadata = std::fs::metadata(&path).map_err(|error| {
            format!(
                "{executable_label} path '{}' could not be read: {error}",
                path.display()
            )
        })?;
        if !metadata.is_file() {
            return Err(format!(
                "{executable_label} path '{}' is not a file",
                path.display()
            ));
        }
        return Ok(Some(path.to_string_lossy().to_string()));
    }

    Ok(Some(trimmed.to_string()))
}

fn looks_like_executable_file_path(value: &str) -> bool {
    value.contains('\\') || value.contains('/') || value.contains(':') || value.starts_with('.')
}

/// Check if live notifications are enabled
pub fn get_live_notifications_enabled() -> bool {
    SETTINGS.lock_or_recover().live_notifications_enabled
}

/// Set live notifications enabled
pub fn set_live_notifications_enabled(enabled: bool) {
    SETTINGS.lock_or_recover().live_notifications_enabled = enabled;
    save_settings();
}

/// Get live notification startup delay in seconds
pub fn get_live_notification_startup_delay_secs() -> u32 {
    SETTINGS
        .lock_or_recover()
        .live_notification_startup_delay_secs
}

/// Set live notification startup delay in seconds
pub fn set_live_notification_startup_delay_secs(delay: u32) {
    SETTINGS
        .lock_or_recover()
        .live_notification_startup_delay_secs = delay;
    save_settings();
}

/// Get custom player path
pub fn get_custom_player_path() -> Option<String> {
    SETTINGS.lock_or_recover().custom_player_path.clone()
}

/// Set custom player path
pub fn set_custom_player_path(path: Option<String>) {
    if let Err(err) = set_custom_player_path_checked(path) {
        tracing::warn!("Rejected custom player path: {err}");
    }
}

/// Validate and set the custom player executable path.
pub fn set_custom_player_path_checked(path: Option<String>) -> Result<Option<String>, String> {
    let normalized = normalize_executable_setting(path, "Custom player executable")?;
    SETTINGS.lock_or_recover().custom_player_path = normalized.clone();
    save_settings();
    Ok(normalized)
}

/// Check if custom player is enabled
pub fn get_use_custom_player() -> bool {
    SETTINGS.lock_or_recover().use_custom_player
}

/// Set custom player enabled
pub fn set_use_custom_player(enabled: bool) {
    SETTINGS.lock_or_recover().use_custom_player = enabled;
    save_settings();
}

/// Get streamlink path
pub fn get_streamlink_path() -> Option<String> {
    SETTINGS.lock_or_recover().streamlink_path.clone()
}

/// Set streamlink path
pub fn set_streamlink_path(path: Option<String>) {
    if let Err(err) = set_streamlink_path_checked(path) {
        tracing::warn!("Rejected Streamlink path: {err}");
    }
}

/// Validate and set the Streamlink executable path.
pub fn set_streamlink_path_checked(path: Option<String>) -> Result<Option<String>, String> {
    let normalized = normalize_executable_setting(path, "Streamlink executable")?;
    SETTINGS.lock_or_recover().streamlink_path = normalized.clone();
    save_settings();
    Ok(normalized)
}

/// Get streamlink quality preset
pub fn get_streamlink_quality() -> StreamlinkQuality {
    SETTINGS.lock_or_recover().streamlink_quality
}

/// Set streamlink quality preset
pub fn set_streamlink_quality(quality: StreamlinkQuality) {
    SETTINGS.lock_or_recover().streamlink_quality = quality;
    save_settings();
}

/// Get custom Streamlink quality text
pub fn get_streamlink_custom_quality() -> String {
    SETTINGS.lock_or_recover().streamlink_custom_quality.clone()
}

/// Set custom Streamlink quality text
pub fn set_streamlink_custom_quality(quality: String) {
    SETTINGS.lock_or_recover().streamlink_custom_quality = quality;
    save_settings();
}

/// Get streamlink extra arguments
pub fn get_streamlink_extra_args() -> String {
    SETTINGS.lock_or_recover().streamlink_extra_args.clone()
}

/// Set streamlink extra arguments
pub fn set_streamlink_extra_args(args: String) {
    SETTINGS.lock_or_recover().streamlink_extra_args = args;
    save_settings();
}

pub fn get_image_uploader_config() -> crate::backend::image_uploader::ImageUploaderConfig {
    SETTINGS.lock_or_recover().image_uploader.clone()
}

pub fn set_image_uploader_config(
    config: crate::backend::image_uploader::ImageUploaderConfig,
) -> Result<(), String> {
    let requires_validation = {
        let settings = SETTINGS.lock_or_recover();
        image_uploader_config_requires_validation(&settings.image_uploader, &config)
    };
    if requires_validation {
        crate::backend::image_uploader::validate_config(&config)?;
    }
    SETTINGS.lock_or_recover().image_uploader = config;
    save_settings();
    Ok(())
}

fn image_uploader_config_requires_validation(
    saved: &crate::backend::image_uploader::ImageUploaderConfig,
    next: &crate::backend::image_uploader::ImageUploaderConfig,
) -> bool {
    saved.request_url != next.request_url
        || saved.form_field != next.form_field
        || saved.extra_headers != next.extra_headers
        || saved.max_upload_bytes != next.max_upload_bytes
        || saved.image_link_template != next.image_link_template
        || saved.deletion_link_template != next.deletion_link_template
        || (!saved.enabled && next.enabled)
}

pub fn get_deleted_message_mode() -> DeletedMessageMode {
    SETTINGS.lock_or_recover().deleted_message_mode
}

pub fn set_deleted_message_mode(mode: DeletedMessageMode) {
    {
        let mut settings = SETTINGS.lock_or_recover();
        if settings.deleted_message_mode == mode {
            return;
        }
        settings.deleted_message_mode = mode;
    }
    bump_layout_settings_version();
    save_settings();
}

#[cfg(test)]
mod security_tests {
    use super::*;
    use core::prelude::v1::test;

    #[test]
    fn export_scrubs_uploader_headers_and_proxy_credentials() {
        let mut settings = AppSettings::default();
        settings.image_uploader.extra_headers = "Authorization: Bearer secret".to_string();
        settings.proxy_url = "http://user:pw@proxy.local:8080".to_string();
        let scrubbed = scrub_secrets_for_export(&settings);
        assert_eq!(scrubbed.image_uploader.extra_headers, "Authorization:");
        assert_eq!(scrubbed.proxy_url, "http://proxy.local:8080");
        assert_eq!(strip_proxy_userinfo("user:pw@proxy:1080"), "proxy:1080");
        assert_eq!(
            strip_proxy_userinfo("socks5://proxy:1080"),
            "socks5://proxy:1080"
        );
    }

    #[test]
    fn import_keeps_current_launch_settings() {
        let current = AppSettings::default();
        let mut imported = AppSettings::default();
        imported.browser_path = Some("C:\\evil.exe".to_string());
        imported.streamlink_extra_args = "--player calc".to_string();
        let skipped = keep_current_sensitive_settings(&mut imported, &current);
        assert_eq!(
            skipped,
            vec!["custom browser", "Streamlink extra arguments"]
        );
        assert_eq!(imported.browser_path, None);
        assert!(imported.streamlink_extra_args.is_empty());
    }

    #[test]
    fn import_keeps_current_paths_uploader_and_proxy() {
        let mut current = AppSettings::default();
        current.highlight_rules.push(HighlightRule {
            id: "r1".to_string(),
            pattern: "hi".to_string(),
            sound_path: Some("C:\\sounds\\ding.wav".to_string()),
            target: HighlightRuleTarget::default(),
            regex: false,
            case_sensitive: false,
            show_in_mentions: true,
            play_sound: true,
            alert: false,
            enabled: true,
            color: None,
        });
        let mut imported = current.clone();
        imported.log_directory = Some("D:\\elsewhere".to_string());
        imported.chat_logging_enabled = !current.chat_logging_enabled;
        imported.theme = Some("Theme File".to_string());
        imported.custom_theme_file_path = "C:\\x.json".to_string();
        imported.highlight_rules[0].sound_path = Some("C:\\evil.wav".to_string());
        imported.image_uploader.request_url = "https://evil.example/up".to_string();
        imported.image_uploader.enabled = true;
        imported.proxy_url = "http://evil:8080".to_string();
        imported.proxy_enabled = true;
        let skipped = keep_current_sensitive_settings(&mut imported, &current);
        assert_eq!(
            skipped,
            vec![
                "chat logging",
                "log directory",
                "theme file",
                "highlight sound files",
                "image uploader enabled",
                "image uploader URL",
                "proxy enabled",
                "proxy URL",
            ]
        );
        assert_eq!(imported.log_directory, current.log_directory);
        assert_eq!(imported.theme, current.theme);
        assert_eq!(imported.highlight_rules, current.highlight_rules);
        assert_eq!(imported.image_uploader, current.image_uploader);
        assert_eq!(imported.proxy_url, current.proxy_url);
    }

    #[test]
    fn remote_and_device_paths_are_detected() {
        for path in [
            "\\\\server\\share\\theme.json",
            "//server/share/x",
            "\\\\?\\C:\\x",
            "\\\\.\\pipe\\x",
            "\\??\\C:\\x",
            "\"\\\\server\\s\"",
            "\\/server/s",
        ] {
            assert!(is_remote_or_device_path(path), "{path}");
        }
        for path in ["C:\\logs", "D:/themes/x.json", "logs", "\\logs", ""] {
            assert!(!is_remote_or_device_path(path), "{path}");
        }
        let mut settings = AppSettings::default();
        settings.custom_player_path = Some("\\\\nas\\tools\\mpv.exe".to_string());
        normalize_runtime_settings(&mut settings);
        assert_eq!(
            settings.custom_player_path.as_deref(),
            Some("\\\\nas\\tools\\mpv.exe")
        );
        assert!(
            normalize_executable_setting(Some("\\\\attacker\\p.exe".to_string()), "Player")
                .is_err()
        );
        assert!(load_custom_theme_file("//attacker/t.json".to_string()).is_err());
    }

    #[test]
    fn export_redacts_uploader_query_values() {
        let mut settings = AppSettings::default();
        settings.image_uploader.request_url = "https://up.example/api?key=secret".to_string();
        let scrubbed = scrub_secrets_for_export(&settings);
        assert_eq!(
            scrubbed.image_uploader.request_url,
            "https://up.example/api?key=REDACTED"
        );
    }

    #[test]
    fn uploader_config_validates_mapping_edits_and_enable_transitions_only() {
        let mut saved = crate::backend::image_uploader::ImageUploaderConfig::default();
        saved.enabled = false;
        saved.request_url = "upload.example.com".to_string();
        saved.extra_headers = "Authorization Bearer legacy".to_string();

        let mut preference_change = saved.clone();
        preference_change.paste_upload_enabled = !saved.paste_upload_enabled;
        assert!(!image_uploader_config_requires_validation(
            &saved,
            &preference_change
        ));

        let mut enabled_saved = saved.clone();
        enabled_saved.enabled = true;
        let mut disable = enabled_saved.clone();
        disable.enabled = false;
        assert!(!image_uploader_config_requires_validation(
            &enabled_saved,
            &disable
        ));

        let mut enable = saved.clone();
        enable.enabled = true;
        assert!(image_uploader_config_requires_validation(&saved, &enable));

        let mut mapping_edit = saved.clone();
        mapping_edit.form_field = "attachment".to_string();
        assert!(image_uploader_config_requires_validation(
            &saved,
            &mapping_edit
        ));
    }

    #[test]
    fn placeholder_uploader_key_is_disabled_during_runtime_normalization() {
        let mut settings = AppSettings::default();
        settings.image_uploader.enabled = true;
        settings.image_uploader.extra_headers = "X-API-Key: YOUR_API_KEY_HERE".to_string();

        normalize_runtime_settings(&mut settings);

        assert!(!settings.image_uploader.enabled);
    }

    #[test]
    fn launch_setting_keys_match_serialized_fields() {
        let value = serde_json::to_value(AppSettings::default()).unwrap();
        for key in LAUNCH_SETTING_KEYS {
            assert!(value.get(*key).is_some(), "missing key {key}");
        }
        let stripped =
            strip_launch_settings_from_json(r#"{"browser_path":"x","font_size":12}"#).unwrap();
        assert!(!stripped.contains("browser_path"));
        assert!(stripped.contains("font_size"));
    }

    #[test]
    fn switch_on_style_defaults_to_neutral_for_older_settings_files() {
        let mut value = serde_json::to_value(AppSettings::default()).unwrap();
        value
            .as_object_mut()
            .expect("settings serialize to an object")
            .remove("switch_on_style");

        let restored: AppSettings = serde_json::from_value(value).unwrap();
        assert_eq!(restored.switch_on_style, SwitchOnStyle::Neutral);
    }

    #[test]
    fn switch_on_style_serializes_with_stable_values() {
        let mut settings = AppSettings::default();
        settings.switch_on_style = SwitchOnStyle::Accent;

        let value = serde_json::to_value(settings).unwrap();
        assert_eq!(value["switch_on_style"], "accent");
    }

    #[test]
    fn future_switch_on_style_falls_back_to_neutral() {
        let mut value = serde_json::to_value(AppSettings::default()).unwrap();
        value["switch_on_style"] = serde_json::json!("future-style");
        let restored: AppSettings = serde_json::from_value(value).unwrap();
        assert_eq!(restored.switch_on_style, SwitchOnStyle::Neutral);
    }
}
