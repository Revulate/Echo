//! Theme settings section with interactive controls wired to backend

use crate::backend::settings::ChatDensity;
use crate::backend::{self, settings};
use crate::components::settings::settings_navigation::SettingDescriptor;
use crate::components::settings::widgets::{
    ColorAlphaMode, ColorEditorOptions, IntensitySliderDrag, SelectOption, SettingsMetrics,
    SettingsPageUi, SettingsSelectState, button_keyed, color_setting_row, labelled_toggle,
    reset_color_editor_draft, setting_float_modified, setting_reset, setting_reset_with_window,
    setting_row, setting_row_full, setting_slider_row, setting_target, settings_defaults,
    settings_disclosure, settings_group, settings_select, slider,
};
use crate::theme::colors;
use crate::theme::tokens::{Radii, RadiiPixels, Spacing};
use gpui::prelude::FluentBuilder;
use gpui::*;
use std::path::Path;
use std::sync::{LazyLock, Mutex, MutexGuard};

const THEME_DETAILS_DISCLOSURE: &str = "appearance-theme-details";
const COLOR_DETAILS_DISCLOSURE: &str = "appearance-more-colors";
const DISPLAY_DETAILS_DISCLOSURE: &str = "appearance-more-display";
const TAB_DETAILS_DISCLOSURE: &str = "appearance-more-tabs";

fn colors_match(left: &str, right: &str) -> bool {
    left.trim().eq_ignore_ascii_case(right.trim())
}

fn optional_colors_match(left: Option<&str>, right: Option<&str>) -> bool {
    match (left, right) {
        (Some(left), Some(right)) => colors_match(left, right),
        (None, None) => true,
        _ => false,
    }
}

const COLOR_THEME: SettingDescriptor = SettingDescriptor {
    id: "appearance.color-theme",
    label: "Theme",
    aliases: &[
        "color theme",
        "dark",
        "light",
        "system",
        "midnight",
        "oled",
        "custom theme",
        "theme file",
        "palette",
    ],
    control_id: "theme",
    disclosures: &[],
};

const THEME_FILE_AUTO_RELOAD: SettingDescriptor = SettingDescriptor {
    id: "appearance.theme-file-auto-reload",
    label: "Auto reload",
    aliases: &["theme file", "watch", "hot reload"],
    control_id: "theme-file-auto-reload",
    disclosures: &[THEME_DETAILS_DISCLOSURE],
};

const CUSTOM_THEME_BACKGROUND_TINT: SettingDescriptor = SettingDescriptor {
    id: "appearance.custom-theme-background-tint",
    label: "Background tint",
    aliases: &[
        "background tint",
        "custom theme",
        "base color",
        "surface color",
    ],
    control_id: "base-color",
    disclosures: &[THEME_DETAILS_DISCLOSURE],
};

const CUSTOM_THEME_MODE: SettingDescriptor = SettingDescriptor {
    id: "appearance.custom-theme-mode",
    label: "Custom theme mode",
    aliases: &["custom theme", "dark", "light"],
    control_id: "custom-mode",
    disclosures: &[THEME_DETAILS_DISCLOSURE],
};

const CUSTOM_THEME_INTENSITY: SettingDescriptor = SettingDescriptor {
    id: "appearance.custom-theme-intensity",
    label: "Custom theme intensity",
    aliases: &["custom theme", "saturation", "intensity"],
    control_id: "saturation",
    disclosures: &[THEME_DETAILS_DISCLOSURE],
};

const THEME_FILE_RELOAD: SettingDescriptor = SettingDescriptor {
    id: "appearance.theme-file-reload",
    label: "Reload theme file now",
    aliases: &["theme file", "refresh", "reload"],
    control_id: "theme-file-reload",
    disclosures: &[THEME_DETAILS_DISCLOSURE],
};

const THEME_FILE_COPY_PATH: SettingDescriptor = SettingDescriptor {
    id: "appearance.theme-file-copy-path",
    label: "Copy theme file path",
    aliases: &["theme file", "clipboard", "path"],
    control_id: "theme-file-copy-path",
    disclosures: &[THEME_DETAILS_DISCLOSURE],
};

const THEME_FILE_OPEN_FOLDER: SettingDescriptor = SettingDescriptor {
    id: "appearance.theme-file-open-folder",
    label: "Open theme file folder",
    aliases: &["theme file", "open", "folder"],
    control_id: "theme-file-open-folder",
    disclosures: &[THEME_DETAILS_DISCLOSURE],
};

const SYSTEM_THEME_REFRESH: SettingDescriptor = SettingDescriptor {
    id: "appearance.system-theme-refresh",
    label: "Refresh system theme",
    aliases: &["system theme", "os theme", "refresh"],
    control_id: "system-theme-refresh",
    disclosures: &[THEME_DETAILS_DISCLOSURE],
};

const PRIMARY_ACCENT: SettingDescriptor = SettingDescriptor {
    id: "appearance.primary-accent",
    label: "Accent color",
    aliases: &["primary accent", "highlight color", "brand color"],
    control_id: "accent",
    disclosures: &[],
};

const PRIMARY_TEXT: SettingDescriptor = SettingDescriptor {
    id: "appearance.primary-text",
    label: "Primary text color",
    aliases: &["primary text", "text color", "foreground", "contrast"],
    control_id: "primary-text",
    disclosures: &[COLOR_DETAILS_DISCLOSURE],
};

const SWITCH_COLOR: SettingDescriptor = SettingDescriptor {
    id: "appearance.switch-color",
    label: "Switch color",
    aliases: &[
        "toggle color",
        "switch accent",
        "switch colour",
        "toggle accent",
    ],
    control_id: "switch-color",
    disclosures: &[COLOR_DETAILS_DISCLOSURE],
};

/// Appearance › Theme & colors.
pub const APPEARANCE_THEME_COLORS_SETTINGS: &[SettingDescriptor] = &[
    COLOR_THEME,
    THEME_FILE_AUTO_RELOAD,
    CUSTOM_THEME_BACKGROUND_TINT,
    CUSTOM_THEME_MODE,
    CUSTOM_THEME_INTENSITY,
    THEME_FILE_RELOAD,
    THEME_FILE_COPY_PATH,
    THEME_FILE_OPEN_FOLDER,
    SYSTEM_THEME_REFRESH,
    PRIMARY_ACCENT,
    PRIMARY_TEXT,
    SWITCH_COLOR,
];

const USERNAME_STYLE: SettingDescriptor = SettingDescriptor {
    id: "appearance.username-style",
    label: "Username style",
    aliases: &["Username Style", "display name", "localized name", "login"],
    control_id: "username-display-mode",
    disclosures: &[],
};

const COLORIZE_USERS: SettingDescriptor = SettingDescriptor {
    id: "appearance.colorize-users",
    label: "Colorize users without a Twitch color",
    aliases: &["gray names", "grey names", "user color"],
    control_id: "colorize-users-without-color",
    disclosures: &["chat-more-reading"],
};

const LIVE_CHATTER_INDICATOR: SettingDescriptor = SettingDescriptor {
    id: "appearance.live-chatter-indicator",
    label: "Show live chatter indicator",
    aliases: &["live streamer", "streaming", "twitch live", "live badge"],
    control_id: "live-chatter-indicator",
    disclosures: &["chat-more-reading"],
};

const UNREAD_INDICATOR_STYLE: SettingDescriptor = SettingDescriptor {
    id: "appearance.unread-indicator",
    label: "Unread tab indicator",
    aliases: &[
        "unread indicator style",
        "badge",
        "dot",
        "unread count",
        "tab badge",
    ],
    control_id: "badge-mode",
    disclosures: &[TAB_DETAILS_DISCLOSURE],
};

const REDUCE_DECORATIVE_MOTION: SettingDescriptor = SettingDescriptor {
    id: "appearance.reduce-decorative-motion",
    label: "Reduce decorative motion",
    aliases: &["accessibility", "reduced motion", "animations", "motion"],
    control_id: "reduce-decorative-motion",
    disclosures: &[],
};

const SMOOTH_SCROLL: SettingDescriptor = SettingDescriptor {
    id: "appearance.smooth-scroll",
    label: "Smooth scrolling",
    aliases: &["animation", "scroll"],
    control_id: "smooth-scroll",
    disclosures: &["chat-more-reading"],
};

const PACED_CHAT_REVEAL: SettingDescriptor = SettingDescriptor {
    id: "appearance.paced-chat-reveal",
    label: "Steady message flow",
    aliases: &["burst", "smooth", "pacing", "live", "busy chat"],
    control_id: "paced-chat-reveal",
    disclosures: &["chat-more-reading"],
};

const JOIN_PART_MESSAGES: SettingDescriptor = SettingDescriptor {
    id: "appearance.join-part-messages",
    label: "Show join/leave messages",
    aliases: &["join", "part", "system message"],
    control_id: "join-part-msgs",
    disclosures: &["chat-more-reading"],
};

const ALTERNATE_MESSAGE_BACKGROUNDS: SettingDescriptor = SettingDescriptor {
    id: "appearance.alternate-message-backgrounds",
    label: "Alternate message backgrounds",
    aliases: &["zebra", "striping", "row shading"],
    control_id: "alternate-msg-bg",
    disclosures: &["chat-more-reading"],
};

/// Chat › Display.
pub const APPEARANCE_CHAT_DISPLAY_SETTINGS: &[SettingDescriptor] = &[
    USERNAME_STYLE,
    COLORIZE_USERS,
    LIVE_CHATTER_INDICATOR,
    SMOOTH_SCROLL,
    PACED_CHAT_REVEAL,
    JOIN_PART_MESSAGES,
    ALTERNATE_MESSAGE_BACKGROUNDS,
];

/// Appearance › Motion.
pub const CHAT_READING_NAME_SETTINGS: &[SettingDescriptor] = &[USERNAME_STYLE];
pub const CHAT_MORE_NAME_SETTINGS: &[SettingDescriptor] = &[COLORIZE_USERS, LIVE_CHATTER_INDICATOR];
pub const CHAT_MORE_MESSAGE_DISPLAY_SETTINGS: &[SettingDescriptor] = &[
    SMOOTH_SCROLL,
    PACED_CHAT_REVEAL,
    ALTERNATE_MESSAGE_BACKGROUNDS,
    JOIN_PART_MESSAGES,
];

/// Appearance › Motion.
pub const APPEARANCE_MOTION_SETTINGS: &[SettingDescriptor] = &[REDUCE_DECORATIVE_MOTION];

const TAB_APPEARANCE: SettingDescriptor = SettingDescriptor {
    id: "appearance.tab-appearance",
    label: "Tab appearance",
    aliases: &[
        "tab appearance",
        "tab style",
        "chrome",
        "firefox",
        "arc",
        "minimal",
    ],
    control_id: "tab-theme",
    disclosures: &[TAB_DETAILS_DISCLOSURE],
};

const TAB_OVERFLOW: SettingDescriptor = SettingDescriptor {
    id: "appearance.tab-overflow",
    label: "Tab overflow",
    aliases: &["tab overflow", "scroll arrows", "dropdown", "multi-line"],
    control_id: "tab-overflow",
    disclosures: &[TAB_DETAILS_DISCLOSURE],
};

const TAB_PLACEMENT: SettingDescriptor = SettingDescriptor {
    id: "appearance.tab-placement",
    label: "Tab placement",
    aliases: &[
        "tab placement",
        "dedicated row",
        "inline title bar",
        "titlebar tabs",
    ],
    control_id: "tab-placement",
    disclosures: &[],
};

const TITLEBAR_DENSITY: SettingDescriptor = SettingDescriptor {
    id: "appearance.titlebar-density",
    label: "Titlebar density",
    aliases: &[
        "titlebar density",
        "title bar height",
        "window controls",
        "compact titlebar",
        "chrome height",
    ],
    control_id: "titlebar-density",
    disclosures: &[TAB_DETAILS_DISCLOSURE],
};

const TEXT_SIZE: SettingDescriptor = SettingDescriptor {
    id: "appearance.text-size",
    label: "Text size",
    aliases: &["text size", "font scale", "font size", "zoom"],
    control_id: "font-scale",
    disclosures: &[],
};

const EMOTE_SIZE: SettingDescriptor = SettingDescriptor {
    id: "appearance.emote-size",
    label: "Emote size",
    aliases: &["emote size", "emote scale"],
    control_id: "emote-scale",
    disclosures: &[],
};

const BADGE_SIZE: SettingDescriptor = SettingDescriptor {
    id: "appearance.badge-size",
    label: "Badge size",
    aliases: &["badge size", "badge scale"],
    control_id: "badge-scale",
    disclosures: &[DISPLAY_DETAILS_DISCLOSURE],
};

const EMOTE_IMAGE_QUALITY: SettingDescriptor = SettingDescriptor {
    id: "appearance.emote-image-quality",
    label: "Emote image quality",
    aliases: &[
        "emote image quality",
        "image scale",
        "1x",
        "2x",
        "4x",
        "resolution",
    ],
    control_id: "emote-image-scale",
    disclosures: &[DISPLAY_DETAILS_DISCLOSURE],
};

const MESSAGE_DENSITY: SettingDescriptor = SettingDescriptor {
    id: "appearance.message-density",
    label: "Message density",
    aliases: &["message density", "compact", "cozy", "spacing", "density"],
    control_id: "chat-density",
    disclosures: &[],
};

/// Appearance › Tabs & sizing.
pub const APPEARANCE_TABS_SIZING_SETTINGS: &[SettingDescriptor] = &[
    UNREAD_INDICATOR_STYLE,
    TAB_APPEARANCE,
    TAB_OVERFLOW,
    TAB_PLACEMENT,
    TITLEBAR_DENSITY,
    TEXT_SIZE,
    EMOTE_SIZE,
    BADGE_SIZE,
    EMOTE_IMAGE_QUALITY,
    MESSAGE_DENSITY,
];

pub const APPEARANCE_THEME_PRIMARY_SETTINGS: &[SettingDescriptor] = &[COLOR_THEME, PRIMARY_ACCENT];

pub const APPEARANCE_TABS_PRIMARY_SETTINGS: &[SettingDescriptor] =
    &[TAB_PLACEMENT, TEXT_SIZE, EMOTE_SIZE, MESSAGE_DENSITY];

pub const APPEARANCE_THEME_OPTIONS_SETTINGS: &[SettingDescriptor] = &[
    THEME_FILE_AUTO_RELOAD,
    CUSTOM_THEME_BACKGROUND_TINT,
    CUSTOM_THEME_MODE,
    CUSTOM_THEME_INTENSITY,
    THEME_FILE_RELOAD,
    THEME_FILE_COPY_PATH,
    THEME_FILE_OPEN_FOLDER,
    SYSTEM_THEME_REFRESH,
    PRIMARY_TEXT,
    SWITCH_COLOR,
    BADGE_SIZE,
    EMOTE_IMAGE_QUALITY,
    TAB_APPEARANCE,
    TAB_OVERFLOW,
    TITLEBAR_DENSITY,
    UNREAD_INDICATOR_STYLE,
];

fn preview_username(
    mode: settings::UsernameDisplayMode,
    login: &str,
    localized_name: &str,
) -> String {
    match mode {
        settings::UsernameDisplayMode::Username => login.to_string(),
        settings::UsernameDisplayMode::LocalizedName => localized_name.to_string(),
        settings::UsernameDisplayMode::UsernameAndLocalizedName => {
            format!("{login}({localized_name})")
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn render_preview_message<M: Into<SharedString>>(
    timestamp: Option<&str>,
    badge_label: Option<&str>,
    username: String,
    username_color: Hsla,
    message: M,
    font_size: f32,
    badge_size: f32,
    row_padding: f32,
    background: Hsla,
    highlight_border: Option<Hsla>,
    emote_size: Option<f32>,
) -> impl IntoElement + use<M> {
    div()
        .w_full()
        .min_w_0()
        .px_2()
        .py(px(row_padding))
        .flex()
        .items_center()
        .gap_1()
        .bg(background)
        .when_some(highlight_border, |row, border| {
            row.border_l(px(2.0)).border_color(border)
        })
        .when_some(timestamp, |row, timestamp| {
            row.child(
                div()
                    .flex_none()
                    .text_size(px((font_size - 2.0).max(9.0)))
                    .text_color(colors::text_muted())
                    .child(timestamp.to_string()),
            )
        })
        .when_some(badge_label, |row, badge_label| {
            row.child(
                div()
                    .flex_none()
                    .size(px(badge_size))
                    .rounded(Radii::sm())
                    .bg(colors::accent_transparent(0.22))
                    .border_1()
                    .border_color(colors::accent_transparent(0.52))
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_size(px((badge_size * 0.5).clamp(7.0, 10.0)))
                    .font_weight(FontWeight::BOLD)
                    .text_color(colors::accent_hover())
                    .child(badge_label.to_string()),
            )
        })
        .child(
            div()
                .min_w_0()
                .flex()
                .items_center()
                .flex_wrap()
                .gap(px(Spacing::XS))
                .text_size(px(font_size))
                .child(
                    div()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(username_color)
                        .child(format!("{username}:")),
                )
                .child(
                    div()
                        .text_color(colors::text_primary())
                        .child(message.into()),
                )
                .when_some(emote_size, |content, emote_size| {
                    content.child(
                        div()
                            .size(px(emote_size))
                            .rounded(Radii::sm())
                            .bg(colors::accent_transparent(0.16))
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(
                                svg()
                                    .path("icons/smile.svg")
                                    .size(px((emote_size - 4.0).max(10.0)))
                                    .text_color(colors::accent_hover()),
                            ),
                    )
                }),
        )
}

fn render_preview_shell(label: &'static str, messages: Vec<AnyElement>) -> impl IntoElement {
    div()
        .id(SharedString::from(format!(
            "settings-preview-{}",
            crate::components::settings::widgets::control_slug(label)
        )))
        .role(Role::Image)
        .aria_label(SharedString::from(format!("{label}, sample chat")))
        .w_full()
        .min_w_0()
        .rounded(Radii::md())
        .overflow_hidden()
        .border_1()
        .border_color(colors::border())
        .bg(colors::bg_void())
        .child(
            div()
                .w_full()
                .h(px(30.0))
                .px_2()
                .flex()
                .items_center()
                .justify_between()
                .bg(colors::bg_elevated())
                .border_b_1()
                .border_color(colors::border_subtle())
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(
                            svg()
                                .path("icons/echo-logo.svg")
                                .size(px(15.0))
                                .text_color(colors::accent_text()),
                        )
                        .child(
                            div()
                                .text_sm()
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(colors::text_primary())
                                .child("#echo"),
                        ),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(colors::text_muted())
                        .child(label),
                ),
        )
        .children(messages)
}

/// A desktop-native preview built from the same semantic Echo language as the
/// iOS app: the waveform mark, restrained accent color, dense chat rows, and
/// immediate feedback. It deliberately uses local primitives instead of a
/// production chat renderer or remote media so Settings stays deterministic.
fn render_appearance_preview(
    font_scale: f32,
    emote_scale: f32,
    badge_scale: f32,
    chat_density: ChatDensity,
    username_display_mode: settings::UsernameDisplayMode,
) -> impl IntoElement {
    let font_size = (13.333 * font_scale).clamp(10.0, 20.0);
    let emote_size = (18.0 * emote_scale).clamp(12.0, 34.0);
    let badge_size = (14.0 * badge_scale).clamp(10.0, 28.0);
    let row_padding = match chat_density {
        ChatDensity::Compact => 1.0,
        ChatDensity::Normal => 3.0,
        ChatDensity::Cozy => 6.0,
    };
    let timestamp = settings::get_show_message_timestamps().then(|| {
        crate::utils::timestamp::format_message_timestamp_with_format(
            1_719_693_240_i64,
            settings::get_message_timestamp_format(),
        )
        .unwrap_or_else(|| "12:34".to_string())
    });
    let alternate_backgrounds = settings::get_alternate_message_backgrounds();

    render_preview_shell(
        "Live appearance preview",
        vec![
            render_preview_message(
                timestamp.as_deref(),
                Some("E"),
                preview_username(username_display_mode, "revulate", "Revulate"),
                colors::accent_hover(),
                "Desktop Echo, tuned your way.",
                font_size,
                badge_size,
                row_padding,
                colors::bg_surface(),
                None,
                None,
            )
            .into_any_element(),
            render_preview_message(
                timestamp.as_deref(),
                None,
                preview_username(username_display_mode, "echoapp", "EchoApp"),
                colors::signal_green(),
                "Settings update as you adjust them.",
                font_size,
                badge_size,
                row_padding,
                colors::accent_transparent(0.14),
                Some(colors::accent()),
                Some(emote_size),
            )
            .into_any_element(),
            render_preview_message(
                timestamp.as_deref(),
                Some("1"),
                preview_username(username_display_mode, "echofan", "Echo Fan"),
                colors::link(),
                "One identity, native on every platform.",
                font_size,
                badge_size,
                row_padding,
                if alternate_backgrounds {
                    colors::bg_alternate_row()
                } else {
                    colors::bg_surface()
                },
                None,
                None,
            )
            .into_any_element(),
        ],
    )
}

/// Sizes and timestamp shared by the highlight preview rows.
struct HighlightPreviewMetrics {
    username_display_mode: settings::UsernameDisplayMode,
    font_size: f32,
    emote_size: f32,
    badge_size: f32,
    row_padding: f32,
    timestamp: Option<String>,
}

fn highlight_preview_metrics() -> HighlightPreviewMetrics {
    let font_scale = get_font_scale();
    let emote_scale = settings::get_emote_scale();
    let badge_scale = settings::get_badge_scale();
    let chat_density = settings::get_chat_density();
    HighlightPreviewMetrics {
        username_display_mode: settings::get_username_display_mode(),
        font_size: (13.333 * font_scale).clamp(10.0, 20.0),
        emote_size: (18.0 * emote_scale).clamp(12.0, 34.0),
        badge_size: (14.0 * badge_scale).clamp(10.0, 28.0),
        row_padding: match chat_density {
            ChatDensity::Compact => 1.0,
            ChatDensity::Normal => 3.0,
            ChatDensity::Cozy => 6.0,
        },
        timestamp: settings::get_show_message_timestamps().then(|| {
            crate::utils::timestamp::format_message_timestamp_with_format(
                1_719_693_240_i64,
                settings::get_message_timestamp_format(),
            )
            .unwrap_or_else(|| "12:34".to_string())
        }),
    }
}

/// One sample row painted with the live treatment for `highlight` (or none),
/// framed like the Highlights preview.
pub(super) fn render_highlight_sample_preview(
    label: &'static str,
    login: &str,
    message: String,
    highlight: Option<&crate::backend::HighlightType>,
) -> impl IntoElement {
    use crate::components::chat::canvas_renderer::layout::{
        HighlightColors, HighlightType as RenderHighlight,
    };

    let metrics = highlight_preview_metrics();
    let highlight = highlight.map(RenderHighlight::from);
    let background = highlight
        .as_ref()
        .and_then(HighlightColors::background_color)
        .unwrap_or_else(gpui::transparent_black);
    let border = highlight.as_ref().and_then(HighlightColors::border_color);
    render_preview_shell(
        label,
        vec![
            render_preview_message(
                metrics.timestamp.as_deref(),
                None,
                preview_username(metrics.username_display_mode, login, login),
                colors::accent_hover(),
                message,
                metrics.font_size,
                metrics.badge_size,
                metrics.row_padding,
                background,
                border,
                None,
            )
            .into_any_element(),
        ],
    )
}

/// Representative messages for Highlights. This consumes the same theme,
/// typography, timestamp, badge, density, and username settings as Appearance,
/// while the row backgrounds and borders come from the live highlight colors.
pub(super) fn render_highlight_preview() -> impl IntoElement {
    let HighlightPreviewMetrics {
        username_display_mode,
        font_size,
        emote_size,
        badge_size,
        row_padding,
        timestamp,
    } = highlight_preview_metrics();

    render_preview_shell(
        "Live highlight preview",
        vec![
            render_preview_message(
                timestamp.as_deref(),
                Some("@"),
                preview_username(username_display_mode, "revulate", "Revulate"),
                colors::accent_hover(),
                "Mentioned you in chat.",
                font_size,
                badge_size,
                row_padding,
                colors::mention_bg(),
                Some(colors::mention_border()),
                None,
            )
            .into_any_element(),
            render_preview_message(
                timestamp.as_deref(),
                None,
                preview_username(username_display_mode, "echoapp", "EchoApp"),
                colors::signal_green(),
                "Replied in a thread you joined.",
                font_size,
                badge_size,
                row_padding,
                colors::thread_highlight_bg(),
                Some(colors::thread_highlight_border()),
                Some(emote_size),
            )
            .into_any_element(),
            render_preview_message(
                timestamp.as_deref(),
                Some("1"),
                preview_username(username_display_mode, "echofan", "Echo Fan"),
                colors::link(),
                "Subscribed to the channel.",
                font_size,
                badge_size,
                row_padding,
                colors::subscription_bg(),
                Some(colors::subscription_border()),
                None,
            )
            .into_any_element(),
        ],
    )
}

/// Current font scale from backend
fn get_font_scale() -> f32 {
    settings::get_font_scale()
}

/// Current tab overflow mode from backend
fn get_tab_overflow_mode() -> String {
    match backend::get_tab_overflow_mode() {
        backend::TabOverflowMode::Scroll => "Scroll".to_string(),
        backend::TabOverflowMode::Dropdown => "Dropdown".to_string(),
        backend::TabOverflowMode::MultiLine => "MultiLine".to_string(),
    }
}

/// Current tab placement from backend.
fn get_tab_placement() -> String {
    match backend::settings::get_tab_placement() {
        backend::settings::TabPlacement::Dedicated => "Dedicated".to_string(),
        backend::settings::TabPlacement::Inline => "Inline".to_string(),
    }
}

/// Current persisted main-titlebar density from backend.
fn get_titlebar_density() -> String {
    match backend::settings::get_titlebar_density() {
        backend::settings::TitlebarDensity::Standard => "Standard".to_string(),
        backend::settings::TitlebarDensity::Compact => "Compact".to_string(),
    }
}

fn normalized_theme_choice(theme: String) -> String {
    [
        "Dark",
        "Light",
        "System",
        "Midnight",
        "Custom",
        "Theme File",
    ]
    .into_iter()
    .find(|choice| theme.eq_ignore_ascii_case(choice))
    .map_or(theme, str::to_string)
}

/// Last probed system-theme status, keyed by the resolved OS theme it describes.
///
/// The probe reads desktop theme config files, so it is re-run only when the
/// resolved theme changes or when the section's Refresh action clears this.
static SYSTEM_THEME_STATUS: LazyLock<Mutex<Option<(settings::ResolvedSystemTheme, String)>>> =
    LazyLock::new(|| Mutex::new(None));

type SystemThemeStatusCache = Option<(settings::ResolvedSystemTheme, String)>;

fn lock_system_theme_status_cache(
    cache: &Mutex<SystemThemeStatusCache>,
) -> MutexGuard<'_, SystemThemeStatusCache> {
    match cache.lock() {
        Ok(guard) => guard,
        Err(poisoned) => {
            let mut guard = poisoned.into_inner();
            *guard = None;
            cache.clear_poison();
            crate::log_ui!(warn, "Reset poisoned system theme status cache");
            guard
        }
    }
}

fn cached_system_theme_status(
    cache: &Mutex<SystemThemeStatusCache>,
    resolved: settings::ResolvedSystemTheme,
) -> Option<String> {
    let cache = lock_system_theme_status_cache(cache);
    cache
        .as_ref()
        .filter(|(cached_resolved, _)| *cached_resolved == resolved)
        .map(|(_, message)| message.clone())
}

fn store_system_theme_status(
    cache: &Mutex<SystemThemeStatusCache>,
    resolved: settings::ResolvedSystemTheme,
    message: String,
) {
    *lock_system_theme_status_cache(cache) = Some((resolved, message));
}

fn get_system_theme_status() -> String {
    let resolved = settings::get_resolved_system_theme();
    if let Some(message) = cached_system_theme_status(&SYSTEM_THEME_STATUS, resolved) {
        return message;
    }

    let message = settings::get_system_theme_status_message();
    store_system_theme_status(&SYSTEM_THEME_STATUS, resolved, message.clone());
    message
}

fn invalidate_system_theme_status() {
    *lock_system_theme_status_cache(&SYSTEM_THEME_STATUS) = None;
}

#[cfg(test)]
mod system_theme_status_cache_tests {
    use super::*;
    use core::prelude::v1::test;
    use std::panic::{AssertUnwindSafe, catch_unwind};

    #[test]
    fn cache_is_keyed_by_resolved_theme() {
        let cache = Mutex::new(None);
        store_system_theme_status(
            &cache,
            settings::ResolvedSystemTheme::Dark,
            "dark status".to_string(),
        );

        assert_eq!(
            cached_system_theme_status(&cache, settings::ResolvedSystemTheme::Dark).as_deref(),
            Some("dark status")
        );
        assert!(cached_system_theme_status(&cache, settings::ResolvedSystemTheme::Light).is_none());
    }

    #[test]
    fn poison_discards_partial_status_and_allows_reuse() {
        let cache = Mutex::new(None);
        let _ = catch_unwind(AssertUnwindSafe(|| {
            let mut cache = cache.lock().expect("test cache should start healthy");
            *cache = Some((
                settings::ResolvedSystemTheme::Dark,
                "partial status".to_string(),
            ));
            panic!("poison test cache");
        }));

        assert!(cache.is_poisoned());
        assert!(cached_system_theme_status(&cache, settings::ResolvedSystemTheme::Dark).is_none());
        assert!(!cache.is_poisoned());

        store_system_theme_status(
            &cache,
            settings::ResolvedSystemTheme::Light,
            "fresh status".to_string(),
        );
        assert_eq!(
            cached_system_theme_status(&cache, settings::ResolvedSystemTheme::Light).as_deref(),
            Some("fresh status")
        );
    }

    #[test]
    fn switch_color_target_stays_in_more_color_options() {
        assert_eq!(SWITCH_COLOR.disclosures, &[COLOR_DETAILS_DISCLOSURE]);
        assert!(SWITCH_COLOR.aliases.contains(&"toggle color"));
        assert!(SWITCH_COLOR.aliases.contains(&"switch accent"));
        assert!(SWITCH_COLOR.aliases.contains(&"switch colour"));
        assert!(
            APPEARANCE_THEME_COLORS_SETTINGS
                .iter()
                .any(|setting| setting.id == SWITCH_COLOR.id)
        );
        assert!(
            APPEARANCE_THEME_OPTIONS_SETTINGS
                .iter()
                .any(|setting| setting.id == SWITCH_COLOR.id)
        );
    }
}

fn get_system_theme_resolution() -> &'static str {
    settings::get_resolved_system_theme().label()
}

fn render_custom_theme_editor<T: 'static>(
    stacked: bool,
    mode_select: SettingsSelectState,
    cx: &mut Context<T>,
) -> impl IntoElement + use<T> {
    let custom = backend::get_custom_theme();
    let defaults = &settings_defaults().custom_theme;
    let default_base_color = defaults.base_color.clone();
    let default_is_light = defaults.is_light;
    let default_saturation = defaults.saturation;

    let base_presets = [
        ("Purple", "#8b5cf6"),
        ("Blue", "#3b82f6"),
        ("Cyan", "#06b6d4"),
        ("Teal", "#14b8a6"),
        ("Green", "#22c55e"),
        ("Lime", "#84cc16"),
        ("Yellow", "#eab308"),
        ("Orange", "#f97316"),
        ("Red", "#ef4444"),
    ];
    let base_color_modified = !colors_match(&custom.base_color, &defaults.base_color);
    let custom_mode_modified = custom.is_light != default_is_light;
    let custom_intensity_modified = setting_float_modified(custom.saturation, default_saturation);

    div()
        .w_full()
        .flex()
        .flex_col()
        .gap(px(Spacing::SM))
        .child(setting_target(
            CUSTOM_THEME_BACKGROUND_TINT,
            color_setting_row(
                CUSTOM_THEME_BACKGROUND_TINT.id,
                CUSTOM_THEME_BACKGROUND_TINT.label,
                Some("Colors surfaces, panels, and borders"),
                CUSTOM_THEME_BACKGROUND_TINT.control_id,
                Some(&custom.base_color),
                ColorEditorOptions {
                    alpha_mode: ColorAlphaMode::Opaque,
                    presets: &base_presets,
                    allow_clear: false,
                    default_value: None,
                },
                base_color_modified,
                base_color_modified.then(|| {
                    setting_reset_with_window(
                        "reset-custom-theme-background-tint",
                        CUSTOM_THEME_BACKGROUND_TINT.label,
                        cx,
                        move |window, cx| {
                            backend::set_custom_theme_base_color(default_base_color.clone());
                            reset_color_editor_draft(
                                CUSTOM_THEME_BACKGROUND_TINT.control_id,
                                Some(&default_base_color),
                                None,
                                ColorAlphaMode::Opaque,
                                window,
                                cx,
                            );
                            cx.notify();
                        },
                    )
                }),
                stacked,
                cx,
                |value, cx| {
                    if let Some(value) = value {
                        backend::set_custom_theme_base_color(value);
                        cx.notify();
                    }
                },
            ),
        ))
        .child(setting_target(
            CUSTOM_THEME_MODE,
            setting_row(
                CUSTOM_THEME_MODE.id,
                "Custom theme mode",
                Some("Generate a dark or light palette"),
                settings_select(
                    CUSTOM_THEME_MODE.control_id,
                    "Custom theme mode",
                    &[
                        SelectOption::new("Dark", "Dark"),
                        SelectOption::new("Light", "Light"),
                    ],
                    if custom.is_light { "Light" } else { "Dark" },
                    &mode_select,
                    cx,
                    |value, cx| {
                        backend::set_custom_theme_light_mode(value == "Light");
                        cx.notify();
                    },
                ),
                custom_mode_modified,
                custom_mode_modified.then(|| {
                    setting_reset(
                        "reset-custom-theme-mode",
                        CUSTOM_THEME_MODE.label,
                        cx,
                        move |cx| {
                            backend::set_custom_theme_light_mode(default_is_light);
                            cx.notify();
                        },
                    )
                }),
                stacked,
            ),
        ))
        .child(setting_target(
            CUSTOM_THEME_INTENSITY,
            setting_slider_row(
                CUSTOM_THEME_INTENSITY.id,
                "Custom theme intensity",
                Some("Adjust the palette saturation"),
                intensity_slider("saturation", custom.saturation, cx, |value, cx| {
                    backend::set_custom_theme_saturation(value);
                    cx.notify();
                }),
                custom_intensity_modified,
                custom_intensity_modified.then(|| {
                    setting_reset(
                        "reset-custom-theme-intensity",
                        CUSTOM_THEME_INTENSITY.label,
                        cx,
                        move |cx| {
                            backend::set_custom_theme_saturation(default_saturation);
                            cx.notify();
                        },
                    )
                }),
                stacked,
            ),
        ))
}

/// Fluid draggable intensity slider
fn intensity_slider<T: 'static, F: Fn(f32, &mut Context<T>) + Clone + 'static>(
    id: &str,
    value: f32,
    cx: &mut Context<T>,
    on_change: F,
) -> impl IntoElement + use<T, F> {
    let normalized = (value / 0.5).clamp(0.0, 1.0);
    let slider_id = id.to_string();
    let slider_id_for_check = slider_id.clone();
    let on_change_drag = on_change.clone();

    let track_height = 8.0_f32;
    let thumb_size = 16.0_f32;

    let track = crate::components::settings::widgets::slider_keyboard(
        div().id(SharedString::from(format!(
            "{}-container",
            slider_id.clone()
        ))),
        "Custom theme intensity",
        value,
        0.0,
        0.5,
        0.01,
        format!("{}%", (value * 100.0).round() as i32),
        cx,
        on_change,
    )
    .w_full()
    .h(px(thumb_size))
    .relative()
    .cursor(CursorStyle::PointingHand)
    .child(
        div()
            .absolute()
            .left_0()
            .right_0()
            .top(px((thumb_size - track_height) / 2.0))
            .h(px(track_height))
            .rounded(px(track_height / 2.0))
            .bg(colors::bg_surface()),
    )
    .child(
        div()
            .absolute()
            .left_0()
            .top(px((thumb_size - track_height) / 2.0))
            .h(px(track_height))
            .w(relative(normalized))
            .rounded(px(track_height / 2.0))
            .bg(colors::accent()),
    )
    .child(
        div()
            .id(SharedString::from(format!("{}-drag", slider_id.clone())))
            .absolute()
            .inset_0()
            .on_drag(
                IntensitySliderDrag {
                    slider_id: slider_id.clone(),
                },
                |drag, _, _, cx| {
                    cx.stop_propagation();
                    cx.new(|_| drag.clone())
                },
            )
            .on_drag_move(cx.listener(
                move |_this, event: &DragMoveEvent<IntensitySliderDrag>, _window, cx| {
                    let drag_state = event.drag(cx);
                    if drag_state.slider_id != slider_id_for_check {
                        return;
                    }
                    let bounds = event.bounds;
                    let mouse_x = event.event.position.x;
                    let relative_x = mouse_x - bounds.origin.x;
                    let width = bounds.size.width;
                    let norm = (relative_x / width).clamp(0.0, 1.0);
                    let actual_value = norm * 0.5;
                    on_change_drag(actual_value, cx);
                },
            ))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |_this, event: &MouseDownEvent, _window, cx| {
                    cx.stop_propagation();
                    let _ = event;
                }),
            ),
    )
    .child(
        div()
            .absolute()
            .top_0()
            .left(relative(normalized))
            .ml(px(-thumb_size / 2.0))
            .w(px(thumb_size))
            .h(px(thumb_size))
            .rounded_full()
            .bg(colors::text_on_accent())
            .border_2()
            .border_color(colors::border())
            .shadow_sm(),
    );
    div()
        .w_full()
        .min_w_0()
        .flex()
        .items_center()
        .gap(px(Spacing::SM))
        .child(div().flex_1().min_w(px(128.0)).child(track))
        .child(
            div()
                .flex_shrink_0()
                .text_sm()
                .text_color(colors::text_primary())
                .child(format!("{}%", (value * 100.0).round() as i32)),
        )
}

/// Appearance › Theme & colors: live preview, theme selection, and the accent /
/// text color editors.
pub fn render_appearance_theme_colors<T: 'static>(
    ui: &mut SettingsPageUi,
    cx: &mut Context<T>,
) -> impl IntoElement + use<T> {
    let appearance = backend::bridge::get_appearance_settings();
    let defaults = settings_defaults();
    let current_theme = normalized_theme_choice(
        appearance
            .theme
            .clone()
            .unwrap_or_else(|| "Dark".to_string()),
    );
    let default_theme =
        normalized_theme_choice(defaults.theme.clone().unwrap_or_else(|| "Dark".to_string()));
    let default_theme_for_reset = default_theme.clone();
    let theme_modified = current_theme != default_theme;
    let accent_color = appearance.primary_accent_color;
    let default_accent_color = defaults.primary_accent_color.clone();
    let accent_modified = !colors_match(&accent_color, &defaults.primary_accent_color);
    let theme_select = ui.select(COLOR_THEME.control_id, cx);

    div()
        .w_full()
        .min_w_0()
        .flex()
        .flex_col()
        .gap(px(SettingsMetrics::GROUP_GAP))
        .child(settings_group(
            "appearance-theme",
            "Appearance",
            false,
            div()
                .flex()
                .flex_col()
                .gap(px(SettingsMetrics::GROUP_GAP))
                .child(setting_target(
                    COLOR_THEME,
                    setting_row(
                        COLOR_THEME.id,
                        "Theme",
                        Some("Choose your preferred color theme"),
                        settings_select(
                            COLOR_THEME.control_id,
                            "Theme",
                            &[
                                SelectOption::new("Dark", "Dark"),
                                SelectOption::new("Light", "Light"),
                                SelectOption::new("System", "System"),
                                SelectOption::new("Midnight", "Midnight"),
                                SelectOption::new("Custom", "Custom"),
                                SelectOption::new("Theme File", "Theme File"),
                            ],
                            &current_theme,
                            &theme_select,
                            cx,
                            |value, cx| {
                                backend::set_theme(&value);
                                cx.notify();
                            },
                        ),
                        theme_modified,
                        theme_modified.then(|| {
                            setting_reset("reset-theme", COLOR_THEME.label, cx, move |cx| {
                                backend::set_theme(&default_theme_for_reset);
                                cx.notify();
                            })
                        }),
                        ui.stacked,
                    ),
                ))
                .child(setting_target(
                    PRIMARY_ACCENT,
                    color_setting_row(
                        PRIMARY_ACCENT.id,
                        PRIMARY_ACCENT.label,
                        Some("Colors buttons, selections, and interactive elements"),
                        PRIMARY_ACCENT.control_id,
                        Some(&accent_color),
                        ColorEditorOptions {
                            alpha_mode: ColorAlphaMode::Opaque,
                            presets: &[
                                ("Purple", "#7c3aed"),
                                ("Blue", "#3b82f6"),
                                ("Green", "#22c55e"),
                                ("Orange", "#f97316"),
                                ("Red", "#ef4444"),
                                ("Pink", "#ec4899"),
                                ("Cyan", "#06b6d4"),
                                ("Amber", "#f59e0b"),
                            ],
                            allow_clear: false,
                            default_value: None,
                        },
                        accent_modified,
                        accent_modified.then(|| {
                            setting_reset_with_window(
                                "reset-primary-accent",
                                PRIMARY_ACCENT.label,
                                cx,
                                move |window, cx| {
                                    settings::set_primary_accent_color(
                                        default_accent_color.clone(),
                                    );
                                    let accent = settings::get_primary_accent_color();
                                    reset_color_editor_draft(
                                        PRIMARY_ACCENT.control_id,
                                        Some(&accent),
                                        None,
                                        ColorAlphaMode::Opaque,
                                        window,
                                        cx,
                                    );
                                    cx.notify();
                                },
                            )
                        }),
                        ui.stacked,
                        cx,
                        |value, cx| {
                            if let Some(value) = value {
                                backend::set_accent_color(&value);
                                cx.notify();
                            }
                        },
                    ),
                )),
        ))
}

fn render_theme_file_controls<T: 'static>(
    stacked: bool,
    cx: &mut Context<T>,
) -> impl IntoElement + use<T> {
    let theme_file_path = settings::get_custom_theme_file_path();
    let default_auto_reload = settings_defaults().custom_theme_file_auto_reload;
    let auto_reload = settings::get_custom_theme_file_auto_reload();
    let auto_reload_modified = auto_reload != default_auto_reload;
    div()
        .flex()
        .flex_col()
        .gap(px(Spacing::SM))
        .child(setting_row(
            "appearance-theme-file-path",
            "Theme file",
            Some("Loaded with /themefile <path-to-theme-json>"),
            div()
                .w_full()
                .min_w_0()
                .text_sm()
                .text_color(colors::text_secondary())
                .text_ellipsis()
                .whitespace_nowrap()
                .id("appearance-theme-file-path-value")
                .aria_description(SharedString::from(theme_file_path.clone()))
                .child(theme_file_path),
            false,
            None,
            stacked,
        ))
        .child(setting_target(
            THEME_FILE_AUTO_RELOAD,
            crate::components::settings::widgets::setting_compact_row(
                THEME_FILE_AUTO_RELOAD.id,
                THEME_FILE_AUTO_RELOAD.label,
                Some("Reload changed theme files while developing"),
                labelled_toggle(
                    THEME_FILE_AUTO_RELOAD.control_id,
                    THEME_FILE_AUTO_RELOAD.label,
                    auto_reload,
                    cx,
                    |enabled, cx| {
                        settings::set_custom_theme_file_auto_reload(enabled);
                        cx.notify();
                    },
                ),
                auto_reload_modified,
                auto_reload_modified.then(|| {
                    setting_reset(
                        "reset-theme-file-auto-reload",
                        THEME_FILE_AUTO_RELOAD.label,
                        cx,
                        move |cx| {
                            settings::set_custom_theme_file_auto_reload(default_auto_reload);
                            cx.notify();
                        },
                    )
                }),
                stacked,
            ),
        ))
        .child(setting_target(
            THEME_FILE_RELOAD,
            setting_row(
                THEME_FILE_RELOAD.id,
                "Reload theme file now",
                Some("Force a reload without waiting for the polling interval"),
                button_keyed("inline-button-reload", "Reload", false, cx, |cx| {
                    match settings::refresh_active_theme() {
                        Ok(detail) => {
                            backend::push_ui_action(backend::UiAction::ShowSuccessToast {
                                message: "Theme file reloaded".to_string(),
                                detail: Some(detail),
                            })
                        }
                        Err(error) => {
                            backend::push_ui_action(backend::UiAction::ShowWarningToast {
                                message: "Could not reload theme file".to_string(),
                                detail: Some(error),
                            })
                        }
                    }
                    cx.notify();
                }),
                false,
                None,
                stacked,
            ),
        ))
        .child(setting_target(
            THEME_FILE_COPY_PATH,
            setting_row(
                THEME_FILE_COPY_PATH.id,
                "Copy theme file path",
                Some("Copies the loaded theme path to the clipboard"),
                button_keyed("inline-button-copy", "Copy", false, cx, |cx| {
                    let path = settings::get_custom_theme_file_path();
                    if path.trim().is_empty() {
                        backend::push_ui_action(backend::UiAction::ShowWarningToast {
                            message: "No theme file path to copy".to_string(),
                            detail: None,
                        });
                    } else {
                        cx.write_to_clipboard(ClipboardItem::new_string(path.clone()));
                        backend::push_ui_action(backend::UiAction::ShowSuccessToast {
                            message: "Theme file path copied".to_string(),
                            detail: Some(path),
                        });
                    }
                    cx.notify();
                }),
                false,
                None,
                stacked,
            ),
        ))
        .child(setting_target(
            THEME_FILE_OPEN_FOLDER,
            setting_row(
                THEME_FILE_OPEN_FOLDER.id,
                "Open theme file folder",
                Some("Opens the folder that contains the active theme file"),
                button_keyed("inline-button-open", "Open", false, cx, |cx| {
                    let path = settings::get_custom_theme_file_path();
                    let folder = Path::new(path.trim()).parent().map(Path::to_path_buf);
                    match folder {
                        Some(folder) if !folder.as_os_str().is_empty() => {
                            crate::utils::open_path(folder.clone(), move |result| {
                                let action = match result {
                                    Ok(()) => backend::UiAction::ShowSuccessToast {
                                        message: "Theme folder opened".to_string(),
                                        detail: Some(folder.display().to_string()),
                                    },
                                    Err(error) => backend::UiAction::ShowWarningToast {
                                        message: "Could not open theme folder".to_string(),
                                        detail: Some(format!("{}: {error}", folder.display())),
                                    },
                                };
                                backend::push_ui_action(action);
                            });
                        }
                        _ => backend::push_ui_action(backend::UiAction::ShowWarningToast {
                            message: "No theme folder available".to_string(),
                            detail: Some("Load a theme file first".to_string()),
                        }),
                    }
                    cx.notify();
                }),
                false,
                None,
                stacked,
            ),
        ))
}

fn render_system_theme_controls<T: 'static>(
    stacked: bool,
    cx: &mut Context<T>,
) -> impl IntoElement + use<T> {
    div()
        .flex()
        .flex_col()
        .gap(px(Spacing::SM))
        .child(setting_row_full(
            "System theme status",
            Some("Echo follows the current desktop theme preference"),
            div()
                .text_sm()
                .text_color(colors::text_secondary())
                .child(get_system_theme_status()),
        ))
        .child(setting_row_full(
            "Last resolved OS theme",
            Some("Updated automatically while Echo is running"),
            div()
                .text_sm()
                .text_color(colors::text_secondary())
                .child(get_system_theme_resolution()),
        ))
        .child(setting_target(
            SYSTEM_THEME_REFRESH,
            setting_row(
                SYSTEM_THEME_REFRESH.id,
                "Refresh system theme",
                Some("Re-check the current OS theme and repaint the app"),
                button_keyed("inline-button-refresh", "Refresh", false, cx, |cx| {
                    invalidate_system_theme_status();
                    match settings::refresh_active_theme() {
                        Ok(detail) => {
                            backend::push_ui_action(backend::UiAction::ShowSuccessToast {
                                message: "System theme refreshed".to_string(),
                                detail: Some(detail),
                            })
                        }
                        Err(error) => {
                            backend::push_ui_action(backend::UiAction::ShowWarningToast {
                                message: "Could not refresh system theme".to_string(),
                                detail: Some(error),
                            })
                        }
                    }
                    cx.notify();
                }),
                false,
                None,
                stacked,
            ),
        ))
}

/// Primary username presentation in the Chat Reading group.
pub fn render_chat_username_style<T: 'static>(
    ui: &mut SettingsPageUi,
    cx: &mut Context<T>,
) -> impl IntoElement + use<T> {
    let current = settings::get_username_display_mode();
    let default = settings_defaults().username_display_mode;
    let modified = current != default;
    let select = ui.select(USERNAME_STYLE.control_id, cx);
    setting_target(
        USERNAME_STYLE,
        setting_row(
            USERNAME_STYLE.id,
            USERNAME_STYLE.label,
            Some("Show Twitch login names, localized names, or both."),
            settings_select(
                USERNAME_STYLE.control_id,
                USERNAME_STYLE.label,
                &[
                    SelectOption::new("username", "Username"),
                    SelectOption::new("localized_name", "Localized name"),
                    SelectOption::new("username_and_localized_name", "Both"),
                ],
                current.settings_value(),
                &select,
                cx,
                |value, cx| {
                    let mode = match value.as_str() {
                        "username" => settings::UsernameDisplayMode::Username,
                        "localized_name" => settings::UsernameDisplayMode::LocalizedName,
                        _ => settings::UsernameDisplayMode::UsernameAndLocalizedName,
                    };
                    settings::set_username_display_mode(mode);
                    cx.notify();
                },
            ),
            modified,
            modified.then(|| {
                setting_reset(
                    "reset-chat-username-style",
                    USERNAME_STYLE.label,
                    cx,
                    move |cx| {
                        settings::set_username_display_mode(default);
                        cx.notify();
                    },
                )
            }),
            ui.stacked,
        ),
    )
}

pub fn render_chat_name_options<T: 'static>(
    stacked: bool,
    cx: &mut Context<T>,
) -> impl IntoElement + use<T> {
    let colorize = settings::get_colorize_users_without_color();
    let live = settings::get_show_live_chatter_indicator();
    let defaults = settings_defaults();
    div()
        .w_full()
        .min_w_0()
        .flex()
        .flex_col()
        .child(chat_presentation_toggle(
            COLORIZE_USERS,
            "Use a stable color when Twitch provides none.",
            colorize,
            defaults.colorize_users_without_color,
            settings::set_colorize_users_without_color,
            stacked,
            cx,
        ))
        .child(chat_presentation_toggle(
            LIVE_CHATTER_INDICATOR,
            "Mark chatters who are live on Twitch.",
            live,
            defaults.show_live_chatter_indicator,
            settings::set_show_live_chatter_indicator,
            stacked,
            cx,
        ))
}

pub fn chat_names_modified_count() -> usize {
    let defaults = settings_defaults();
    usize::from(
        settings::get_colorize_users_without_color() != defaults.colorize_users_without_color,
    ) + usize::from(
        settings::get_show_live_chatter_indicator() != defaults.show_live_chatter_indicator,
    )
}

#[allow(clippy::too_many_arguments)]
fn chat_presentation_toggle<T: 'static>(
    descriptor: SettingDescriptor,
    help: &str,
    current: bool,
    default: bool,
    setter: fn(bool),
    stacked: bool,
    cx: &mut Context<T>,
) -> impl IntoElement + use<T> {
    let modified = current != default;
    setting_target(
        descriptor,
        crate::components::settings::widgets::setting_compact_row(
            descriptor.id,
            descriptor.label,
            Some(help),
            labelled_toggle(
                descriptor.control_id,
                descriptor.label,
                current,
                cx,
                move |value, cx| {
                    setter(value);
                    cx.notify();
                },
            ),
            modified,
            modified.then(|| {
                setting_reset(
                    format!("reset-{}", descriptor.id),
                    descriptor.label,
                    cx,
                    move |cx| {
                        setter(default);
                        cx.notify();
                    },
                )
            }),
            stacked,
        ),
    )
}

pub fn chat_message_display_modified_count() -> usize {
    let defaults = settings_defaults();
    usize::from(settings::get_smooth_scroll_enabled() != defaults.smooth_scroll_enabled)
        + usize::from(settings::get_paced_chat_reveal() != defaults.paced_chat_reveal)
        + usize::from(settings::get_show_join_part_messages() != defaults.show_join_part_messages)
        + usize::from(
            settings::get_alternate_message_backgrounds() != defaults.alternate_message_backgrounds,
        )
}

pub fn render_chat_message_display_options<T: 'static>(
    stacked: bool,
    cx: &mut Context<T>,
) -> impl IntoElement + use<T> {
    let defaults = settings_defaults();
    div()
        .w_full()
        .min_w_0()
        .flex()
        .flex_col()
        .child(chat_presentation_toggle(
            SMOOTH_SCROLL,
            "Animate scrolling between chat positions.",
            settings::get_smooth_scroll_enabled(),
            defaults.smooth_scroll_enabled,
            settings::set_smooth_scroll_enabled,
            stacked,
            cx,
        ))
        .child(chat_presentation_toggle(
            PACED_CHAT_REVEAL,
            "When Twitch delivers a busy chat in bursts, show them at a steady pace (up to 2.5 s behind).",
            settings::get_paced_chat_reveal(),
            defaults.paced_chat_reveal,
            settings::set_paced_chat_reveal,
            stacked,
            cx,
        ))
        .child(chat_presentation_toggle(
            ALTERNATE_MESSAGE_BACKGROUNDS,
            "Shade alternate messages for easier reading.",
            settings::get_alternate_message_backgrounds(),
            defaults.alternate_message_backgrounds,
            settings::set_alternate_message_backgrounds,
            stacked,
            cx,
        ))
        .child(chat_presentation_toggle(
            JOIN_PART_MESSAGES,
            "Show when users join or leave the channel.",
            settings::get_show_join_part_messages(),
            defaults.show_join_part_messages,
            settings::set_show_join_part_messages,
            stacked,
            cx,
        ))
}

/// Appearance › Motion.
pub fn render_appearance_motion<T: 'static>(
    ui: &mut SettingsPageUi,
    cx: &mut Context<T>,
) -> impl IntoElement + use<T> {
    let reduce_decorative_motion = settings::get_reduce_decorative_motion();
    let default = settings_defaults().reduce_decorative_motion;
    let modified = reduce_decorative_motion != default;

    settings_group(
        "appearance-motion",
        "Motion",
        false,
        div().flex().flex_col().gap_2().child(setting_target(
            REDUCE_DECORATIVE_MOTION,
            crate::components::settings::widgets::setting_compact_row(
                REDUCE_DECORATIVE_MOTION.id,
                REDUCE_DECORATIVE_MOTION.label,
                Some("Keep interface effects static; chat and picker playback stay separate"),
                labelled_toggle(
                    SharedString::from(format!("toggle-{}", REDUCE_DECORATIVE_MOTION.control_id)),
                    REDUCE_DECORATIVE_MOTION.label,
                    reduce_decorative_motion,
                    cx,
                    |new_state, cx| {
                        settings::set_reduce_decorative_motion(new_state);
                        cx.notify();
                    },
                ),
                modified,
                modified.then(|| {
                    setting_reset(
                        "reset-reduce-decorative-motion",
                        REDUCE_DECORATIVE_MOTION.label,
                        cx,
                        move |cx| {
                            settings::set_reduce_decorative_motion(default);
                            cx.notify();
                        },
                    )
                }),
                ui.stacked,
            ),
        )),
    )
}

/// Appearance › Tabs & sizing: tab chrome plus every scale/density control.
pub fn render_appearance_tabs_sizing<T: 'static>(
    ui: &mut SettingsPageUi,
    cx: &mut Context<T>,
) -> impl IntoElement + use<T> {
    let appearance = backend::bridge::get_appearance_settings();
    let defaults = settings_defaults();
    let font_scale = appearance.font_scale;
    let tab_placement = get_tab_placement();
    let emote_scale = settings::get_emote_scale();
    let badge_scale = settings::get_badge_scale();
    let chat_density = settings::get_chat_density();
    let tab_placement_select = ui.select(TAB_PLACEMENT.control_id, cx);
    let density_select = ui.select(MESSAGE_DENSITY.control_id, cx);
    let tab_placement_modified = tab_placement != format!("{:?}", defaults.tab_placement);
    let text_size_modified = setting_float_modified(font_scale, defaults.font_scale);
    let emote_size_modified = setting_float_modified(emote_scale, defaults.emote_scale);
    let density_modified = chat_density != defaults.chat_density;

    div()
        .w_full()
        .min_w_0()
        .flex()
        .flex_col()
        .gap(px(SettingsMetrics::GROUP_GAP))
        .child(settings_group(
            "appearance-sizing",
            "Sizing and tabs",
            false,
            div()
                .flex()
                .flex_col()
                .gap(px(Spacing::SM))
                .child(setting_target(TAB_PLACEMENT, setting_row(
                    TAB_PLACEMENT.id,
                    "Tab placement",
                    Some(
                        "Use a dedicated row or the title bar; multi-line overflow uses the row.",
                    ),
                    settings_select(
                        TAB_PLACEMENT.control_id,
                        "Tab placement",
                        &[
                            SelectOption::new("Dedicated", "Dedicated Row"),
                            SelectOption::new("Inline", "Inline Title Bar"),
                        ],
                        &tab_placement,
                        &tab_placement_select,
                        cx,
                        |value, cx| {
                            let placement = match value.as_str() {
                                "Inline" => backend::settings::TabPlacement::Inline,
                                _ => backend::settings::TabPlacement::Dedicated,
                            };
                            backend::settings::set_tab_placement(placement);
                            cx.notify();
                        },
                    ),
                    tab_placement_modified,
                    tab_placement_modified.then(|| {
                        setting_reset("reset-tab-placement", TAB_PLACEMENT.label, cx, move |cx| {
                            settings::set_tab_placement(settings_defaults().tab_placement.clone());
                            cx.notify();
                        })
                    }),
                    ui.stacked,
                )))
                .child(setting_target(TEXT_SIZE, setting_slider_row(
                    TEXT_SIZE.id,
                    "Text size",
                    Some("Scale text in chat messages"),
                    slider(
                        TEXT_SIZE.control_id,
                        font_scale,
                        0.75,
                        1.5,
                        0.05,
                        |v| format!("{}%", (v * 100.0).round() as i32),
                        cx,
                        |value, cx| {
                            crate::log_ui!(debug, value, "UI slider changed: font-scale");
                            backend::set_font_scale(value);
                            cx.notify();
                        },
                    )
                    .aria_label(SharedString::from(TEXT_SIZE.label)),
                    text_size_modified,
                    text_size_modified.then(|| {
                        setting_reset("reset-text-size", TEXT_SIZE.label, cx, move |cx| {
                            settings::set_font_scale(settings_defaults().font_scale);
                            cx.notify();
                        })
                    }),
                    ui.stacked,
                )))
                .child(setting_target(EMOTE_SIZE, setting_slider_row(
                    EMOTE_SIZE.id,
                    "Emote size",
                    Some("Scale emotes in chat messages"),
                    slider(
                        EMOTE_SIZE.control_id,
                        emote_scale,
                        0.5,
                        2.0,
                        0.05,
                        |v| format!("{}%", (v * 100.0).round() as i32),
                        cx,
                        |value, cx| {
                            crate::log_ui!(debug, value, "UI slider changed: emote-scale");
                            settings::set_emote_scale(value);
                            cx.notify();
                        },
                    )
                    .aria_label(SharedString::from(EMOTE_SIZE.label)),
                    emote_size_modified,
                    emote_size_modified.then(|| {
                        setting_reset("reset-emote-size", EMOTE_SIZE.label, cx, move |cx| {
                            settings::set_emote_scale(settings_defaults().emote_scale);
                            cx.notify();
                        })
                    }),
                    ui.stacked,
                )))
                .child(setting_target(MESSAGE_DENSITY, setting_row(
                    MESSAGE_DENSITY.id,
                    "Message density",
                    Some("Adjust spacing between chat messages"),
                    settings_select(
                        MESSAGE_DENSITY.control_id,
                        "Message density",
                        &[
                            SelectOption::new("Compact", "Compact"),
                            SelectOption::new("Normal", "Normal"),
                            SelectOption::new("Cozy", "Cozy"),
                        ],
                        match chat_density { ChatDensity::Compact => "Compact", ChatDensity::Normal => "Normal", ChatDensity::Cozy => "Cozy" },
                        &density_select,
                        cx,
                        |value, cx| {
                            settings::set_chat_density(match value.as_str() { "Compact" => ChatDensity::Compact, "Cozy" => ChatDensity::Cozy, _ => ChatDensity::Normal });
                            cx.notify();
                        },
                    ),
                    density_modified,
                    density_modified.then(|| {
                        setting_reset("reset-message-density", MESSAGE_DENSITY.label, cx, move |cx| {
                            settings::set_chat_density(settings_defaults().chat_density);
                            cx.notify();
                        })
                    }),
                    ui.stacked,
                )))
                .child(settings_group(
                    "appearance-preview",
                    "Live preview",
                    true,
                    render_appearance_preview(
                        font_scale,
                        emote_scale,
                        badge_scale,
                        chat_density,
                        settings::get_username_display_mode(),
                    ),
                )),
        ))
}

/// Collapsed Appearance controls shared by the final Appearance page block.
pub fn render_appearance_more<T: 'static>(
    ui: &mut SettingsPageUi,
    cx: &mut Context<T>,
) -> impl IntoElement + use<T> {
    let defaults = settings_defaults();
    let stacked = ui.stacked;
    let current_theme = settings::get_theme().unwrap_or_else(|| "Dark".to_string());
    let is_custom = current_theme.eq_ignore_ascii_case("custom");
    let is_theme_file = current_theme.eq_ignore_ascii_case("theme file");
    let custom_theme = settings::get_custom_theme();
    let primary_text_color = settings::get_primary_text_color();
    let switch_on_style = settings::get_switch_on_style();
    let switch_color_modified = switch_on_style != settings::SwitchOnStyle::Neutral;
    let badge_scale = settings::get_badge_scale();
    let emote_image_scale = settings::get_emote_image_scale();
    let appearance = backend::bridge::get_appearance_settings();
    let tab_theme = format!("{:?}", appearance.tab_theme);
    let tab_overflow = get_tab_overflow_mode();
    let titlebar_density = get_titlebar_density();
    let badge_mode = format!("{:?}", appearance.badge_display_mode);
    let theme_details = ui.disclosure(THEME_DETAILS_DISCLOSURE, cx);
    let has_theme_details = is_custom
        || is_theme_file
        || current_theme.eq_ignore_ascii_case("system")
        || (ui.searching && theme_details.is_search_revealed());
    let color_details = ui.disclosure(COLOR_DETAILS_DISCLOSURE, cx);
    let display_details = ui.disclosure(DISPLAY_DETAILS_DISCLOSURE, cx);
    let tab_details = ui.disclosure(TAB_DETAILS_DISCLOSURE, cx);
    let custom_mode_select = ui.select(CUSTOM_THEME_MODE.control_id, cx);
    let emote_quality_select = ui.select(EMOTE_IMAGE_QUALITY.control_id, cx);
    let tab_theme_select = ui.select(TAB_APPEARANCE.control_id, cx);
    let tab_overflow_select = ui.select(TAB_OVERFLOW.control_id, cx);
    let titlebar_density_select = ui.select(TITLEBAR_DENSITY.control_id, cx);
    let unread_style_select = ui.select(UNREAD_INDICATOR_STYLE.control_id, cx);
    let theme_details_modified = usize::from(
        is_custom && !colors_match(&custom_theme.base_color, &defaults.custom_theme.base_color),
    ) + usize::from(
        is_custom && custom_theme.is_light != defaults.custom_theme.is_light,
    ) + usize::from(
        is_custom
            && setting_float_modified(custom_theme.saturation, defaults.custom_theme.saturation),
    ) + usize::from(
        is_theme_file
            && settings::get_custom_theme_file_auto_reload()
                != defaults.custom_theme_file_auto_reload,
    );
    let display_details_modified =
        usize::from(setting_float_modified(badge_scale, defaults.badge_scale))
            + usize::from(emote_image_scale != defaults.emote_image_scale);
    let tab_details_modified = usize::from(tab_theme != format!("{:?}", defaults.tab_theme))
        + usize::from(tab_overflow != format!("{:?}", defaults.tab_overflow_mode))
        + usize::from(titlebar_density != format!("{:?}", defaults.titlebar_density))
        + usize::from(badge_mode != format!("{:?}", defaults.badge_display_mode));

    settings_group(
        "appearance-more",
        "More appearance options",
        false,
        div()
            .flex()
            .flex_col()
            .gap(px(Spacing::SM))
            .when(has_theme_details, |content| {
                content.child(settings_disclosure(
                    THEME_DETAILS_DISCLOSURE,
                    "Theme details",
                    Some("Custom, system, and file controls"),
                    theme_details_modified,
                    &theme_details,
                    cx,
                    move |cx| {
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(Spacing::SM))
                            .children(is_custom.then(|| {
                                render_custom_theme_editor(
                                    stacked,
                                    custom_mode_select.clone(),
                                    cx,
                                )
                            }))
                            .children(is_theme_file.then(|| {
                                render_theme_file_controls(stacked, cx)
                            }))
                            .children(current_theme.eq_ignore_ascii_case("system").then(|| {
                                render_system_theme_controls(stacked, cx)
                            }))
                    },
                ))
            })
            .child(settings_disclosure(
                COLOR_DETAILS_DISCLOSURE,
                "More color options",
                Some("Primary text and switch color"),
                usize::from(!optional_colors_match(
                    primary_text_color.as_deref(),
                    defaults.primary_text_color.as_deref(),
                )) + usize::from(switch_color_modified),
                &color_details,
                cx,
                move |cx| {
                    let primary_text_modified = !optional_colors_match(
                        primary_text_color.as_deref(),
                        defaults.primary_text_color.as_deref(),
                    );
                    let switch_color_modified = switch_on_style != settings::SwitchOnStyle::Neutral;
                    div()
                            .flex()
                            .flex_col()
                            .gap(px(Spacing::SM))
                            .child(setting_target(
                                PRIMARY_TEXT,
                                color_setting_row(
                                PRIMARY_TEXT.id,
                                PRIMARY_TEXT.label,
                                Some("Override primary UI and chat text; clear to follow the active theme"),
                                PRIMARY_TEXT.control_id,
                                primary_text_color.as_deref(),
                                ColorEditorOptions {
                                    alpha_mode: ColorAlphaMode::Opaque,
                                    presets: &[
                                        ("White", "#FFFFFF"),
                                        ("Soft White", "#E8E8EA"),
                                        ("Warm", "#FFF4E6"),
                                        ("Cool", "#E8F1FF"),
                                        ("Slate", "#CBD5E1"),
                                        ("Black", "#141414"),
                                    ],
                                    allow_clear: true,
                                    default_value: None,
                                },
                                primary_text_modified,
                                primary_text_modified.then(|| {
                                    setting_reset_with_window(
                                        "reset-primary-text",
                                        PRIMARY_TEXT.label,
                                        cx,
                                        move |window, cx| {
                                            settings::set_primary_text_color(None);
                                            let inherited = format!(
                                                "#{:06X}",
                                                u32::from(colors::inherited_text_primary().to_rgb())
                                                    >> 8
                                            );
                                            reset_color_editor_draft(
                                                PRIMARY_TEXT.control_id,
                                                None,
                                                Some(inherited.as_str()),
                                                ColorAlphaMode::Opaque,
                                                window,
                                                cx,
                                            );
                                            cx.notify();
                                        },
                                    )
                                }),
                                stacked,
                                cx,
                                |value, cx| {
                                    settings::set_primary_text_color(value);
                                    cx.notify();
                                },
                                ),
                            ))
                            .child(setting_target(
                                SWITCH_COLOR,
                                crate::components::settings::widgets::setting_compact_row(
                                    SWITCH_COLOR.id,
                                    SWITCH_COLOR.label,
                                    Some("Choose whether enabled switches use a neutral or accent track"),
                                    crate::components::settings::widgets::select_buttons(
                                        SWITCH_COLOR.control_id,
                                        &[
                                            SelectOption::new("neutral", "Neutral"),
                                            SelectOption::new("accent", "Accent"),
                                        ],
                                        match switch_on_style {
                                            settings::SwitchOnStyle::Neutral => "neutral",
                                            settings::SwitchOnStyle::Accent => "accent",
                                        },
                                        cx,
                                        |value, cx| {
                                            settings::set_switch_on_style(match value.as_str() {
                                                "accent" => settings::SwitchOnStyle::Accent,
                                                _ => settings::SwitchOnStyle::Neutral,
                                            });
                                            cx.notify();
                                        },
                                    ),
                                    switch_color_modified,
                                    switch_color_modified.then(|| {
                                        setting_reset(
                                            "reset-switch-color",
                                            SWITCH_COLOR.label,
                                            cx,
                                            |cx| {
                                                settings::set_switch_on_style(
                                                    settings::SwitchOnStyle::Neutral,
                                                );
                                                cx.notify();
                                            },
                                        )
                                    }),
                                    stacked,
                                ),
                            ))
                },
            ))
            .child(settings_disclosure(
                DISPLAY_DETAILS_DISCLOSURE,
                "More display options",
                Some("Badge scale and emote image quality"),
                display_details_modified,
                &display_details,
                cx,
                move |cx| {
                    render_display_details(
                        badge_scale,
                        emote_image_scale,
                        stacked,
                        emote_quality_select.clone(),
                        cx,
                    )
                },
            ))
            .child(settings_disclosure(
                TAB_DETAILS_DISCLOSURE,
                "More tab options",
                Some("Appearance, overflow, title bar, and unread style"),
                tab_details_modified,
                &tab_details,
                cx,
                move |cx| {
                    render_tab_details(
                        tab_theme.clone(),
                        tab_overflow.clone(),
                        titlebar_density.clone(),
                        badge_mode.clone(),
                        stacked,
                        tab_theme_select.clone(),
                        tab_overflow_select.clone(),
                        titlebar_density_select.clone(),
                        unread_style_select.clone(),
                        cx,
                    )
                },
            )),
    )
}

fn render_display_details<T: 'static>(
    badge_scale: f32,
    emote_image_scale: settings::EmoteImageScale,
    stacked: bool,
    emote_quality_select: SettingsSelectState,
    cx: &mut Context<T>,
) -> impl IntoElement + use<T> {
    let defaults = settings_defaults();
    let badge_size_modified = setting_float_modified(badge_scale, defaults.badge_scale);
    let emote_quality_modified = emote_image_scale != defaults.emote_image_scale;
    div()
        .flex()
        .flex_col()
        .gap(px(Spacing::SM))
        .child(setting_target(
            BADGE_SIZE,
            setting_slider_row(
                BADGE_SIZE.id,
                "Badge size",
                Some("Scale chat badges independently from text size"),
                slider(
                    BADGE_SIZE.control_id,
                    badge_scale,
                    0.5,
                    3.0,
                    0.05,
                    |v| format!("{}%", (v * 100.0).round() as i32),
                    cx,
                    |value, cx| {
                        crate::log_ui!(debug, value, "UI slider changed: badge-scale");
                        settings::set_badge_scale(value);
                        cx.notify();
                    },
                )
                .aria_label(SharedString::from(BADGE_SIZE.label)),
                badge_size_modified,
                badge_size_modified.then(|| {
                    setting_reset("reset-badge-size", BADGE_SIZE.label, cx, move |cx| {
                        settings::set_badge_scale(settings_defaults().badge_scale);
                        cx.notify();
                    })
                }),
                stacked,
            ),
        ))
        .child(setting_target(
            EMOTE_IMAGE_QUALITY,
            setting_row(
                EMOTE_IMAGE_QUALITY.id,
                "Emote image quality",
                Some("Auto chooses the smallest crisp source for this display and emote size"),
                settings_select(
                    EMOTE_IMAGE_QUALITY.control_id,
                    "Emote image quality",
                    &[
                        SelectOption::new("auto", "Auto"),
                        SelectOption::new("1x", "1x"),
                        SelectOption::new("2x", "2x"),
                        SelectOption::new("4x", "4x"),
                    ],
                    emote_image_scale.settings_value(),
                    &emote_quality_select,
                    cx,
                    |value, cx| {
                        settings::set_emote_image_scale(
                            settings::EmoteImageScale::from_settings_value(&value),
                        );
                        cx.notify();
                    },
                ),
                emote_quality_modified,
                emote_quality_modified.then(|| {
                    setting_reset(
                        "reset-emote-image-quality",
                        EMOTE_IMAGE_QUALITY.label,
                        cx,
                        move |cx| {
                            settings::set_emote_image_scale(settings_defaults().emote_image_scale);
                            cx.notify();
                        },
                    )
                }),
                stacked,
            ),
        ))
}

#[allow(clippy::too_many_arguments)]
fn render_tab_details<T: 'static>(
    tab_theme: String,
    tab_overflow: String,
    titlebar_density: String,
    badge_mode: String,
    stacked: bool,
    tab_theme_select: SettingsSelectState,
    tab_overflow_select: SettingsSelectState,
    titlebar_density_select: SettingsSelectState,
    unread_style_select: SettingsSelectState,
    cx: &mut Context<T>,
) -> impl IntoElement + use<T> {
    let defaults = settings_defaults();
    let tab_appearance_modified = tab_theme != format!("{:?}", defaults.tab_theme);
    let tab_overflow_modified = tab_overflow != format!("{:?}", defaults.tab_overflow_mode);
    let titlebar_density_modified = titlebar_density != format!("{:?}", defaults.titlebar_density);
    let unread_indicator_modified = badge_mode != format!("{:?}", defaults.badge_display_mode);
    div()
        .flex()
        .flex_col()
        .gap(px(Spacing::SM))
        .child(setting_target(
            TAB_APPEARANCE,
            setting_row(
                TAB_APPEARANCE.id,
                "Tab appearance",
                Some("Choose the visual style for navigation tabs"),
                settings_select(
                    TAB_APPEARANCE.control_id,
                    "Tab appearance",
                    &[
                        SelectOption::new("Chrome", "Chrome"),
                        SelectOption::new("Firefox", "Firefox"),
                        SelectOption::new("Edge", "Edge"),
                        SelectOption::new("Square", "Square"),
                        SelectOption::new("Arc", "Arc"),
                        // The value mirrors the variant's `Debug` name.
                        SelectOption::new("Minimal", "Minimal"),
                    ],
                    &tab_theme,
                    &tab_theme_select,
                    cx,
                    |value, cx| {
                        let theme = match value.as_str() {
                            "Firefox" => backend::TabTheme::Firefox,
                            "Edge" => backend::TabTheme::Edge,
                            "Square" => backend::TabTheme::Square,
                            "Arc" => backend::TabTheme::Arc,
                            "Minimal" => backend::TabTheme::Minimal,
                            _ => backend::TabTheme::Chrome,
                        };
                        backend::set_tab_theme(theme);
                        cx.notify();
                    },
                ),
                tab_appearance_modified,
                tab_appearance_modified.then(|| {
                    setting_reset(
                        "reset-tab-appearance",
                        TAB_APPEARANCE.label,
                        cx,
                        move |cx| {
                            settings::set_tab_theme(settings_defaults().tab_theme.clone());
                            cx.notify();
                        },
                    )
                }),
                stacked,
            ),
        ))
        .child(setting_target(
            TAB_OVERFLOW,
            setting_row(
                TAB_OVERFLOW.id,
                "Tab overflow",
                Some("How tabs behave when they do not fit in the window"),
                settings_select(
                    TAB_OVERFLOW.control_id,
                    "Tab overflow",
                    &[
                        SelectOption::new("Scroll", "Scroll arrows"),
                        SelectOption::new("Dropdown", "Dropdown menu"),
                        SelectOption::new("MultiLine", "Multi-line"),
                    ],
                    &tab_overflow,
                    &tab_overflow_select,
                    cx,
                    |value, cx| {
                        let mode = match value.as_str() {
                            "Dropdown" => backend::TabOverflowMode::Dropdown,
                            "MultiLine" => backend::TabOverflowMode::MultiLine,
                            _ => backend::TabOverflowMode::Scroll,
                        };
                        backend::set_tab_overflow_mode(mode);
                        cx.notify();
                    },
                ),
                tab_overflow_modified,
                tab_overflow_modified.then(|| {
                    setting_reset("reset-tab-overflow", TAB_OVERFLOW.label, cx, move |cx| {
                        settings::set_tab_overflow_mode(
                            settings_defaults().tab_overflow_mode.clone(),
                        );
                        cx.notify();
                    })
                }),
                stacked,
            ),
        ))
        .child(setting_target(
            TITLEBAR_DENSITY,
            setting_row(
                TITLEBAR_DENSITY.id,
                "Titlebar density",
                Some("Compact uses 28 px chrome; Standard uses 36 px"),
                settings_select(
                    TITLEBAR_DENSITY.control_id,
                    "Titlebar density",
                    &[
                        SelectOption::new("Compact", "Compact (28 px)"),
                        SelectOption::new("Standard", "Standard (36 px)"),
                    ],
                    &titlebar_density,
                    &titlebar_density_select,
                    cx,
                    |value, cx| {
                        let density = match value.as_str() {
                            "Standard" => settings::TitlebarDensity::Standard,
                            _ => settings::TitlebarDensity::Compact,
                        };
                        settings::set_titlebar_density(density);
                        cx.notify();
                    },
                ),
                titlebar_density_modified,
                titlebar_density_modified.then(|| {
                    setting_reset(
                        "reset-titlebar-density",
                        TITLEBAR_DENSITY.label,
                        cx,
                        move |cx| {
                            settings::set_titlebar_density(settings_defaults().titlebar_density);
                            cx.notify();
                        },
                    )
                }),
                stacked,
            ),
        ))
        .child(setting_target(
            UNREAD_INDICATOR_STYLE,
            setting_row(
                UNREAD_INDICATOR_STYLE.id,
                "Unread tab indicator",
                Some("How unread messages are shown on tabs"),
                settings_select(
                    UNREAD_INDICATOR_STYLE.control_id,
                    "Unread tab indicator",
                    &[
                        SelectOption::new("Number", "Number badges"),
                        SelectOption::new("Dot", "Dot indicator"),
                        SelectOption::new("Highlight", "Highlight only"),
                    ],
                    &badge_mode,
                    &unread_style_select,
                    cx,
                    |value, cx| {
                        let mode = match value.as_str() {
                            "Dot" => backend::BadgeDisplayMode::Dot,
                            "Highlight" => backend::BadgeDisplayMode::Highlight,
                            _ => backend::BadgeDisplayMode::Number,
                        };
                        backend::set_badge_display_mode(mode);
                        cx.notify();
                    },
                ),
                unread_indicator_modified,
                unread_indicator_modified.then(|| {
                    setting_reset(
                        "reset-unread-tab-indicator",
                        UNREAD_INDICATOR_STYLE.label,
                        cx,
                        move |cx| {
                            settings::set_badge_display_mode(
                                settings_defaults().badge_display_mode.clone(),
                            );
                            cx.notify();
                        },
                    )
                }),
                stacked,
            ),
        ))
}
