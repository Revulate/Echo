//! General/Account settings section with interactive controls wired to backend

use super::single_line_text;
use crate::backend::{
    self, Account,
    settings::{
        ChatHoverPauseMode, ChatPauseModifier, MAX_TIMESTAMP_LITERAL_CHARS, StreamlinkQuality,
        TIMESTAMP_LITERAL_PRESETS, TIMESTAMP_PATTERN_VERSION, TimestampPattern,
        TimestampResolution, TimestampToken, UsernameRightClickBehavior,
        UsernameRightClickModifier, validate_timestamp_literal,
    },
};
use crate::components::auth::{LoginModalState, render_profile_avatar};
use crate::components::settings::draft_input::{DraftField, DraftInput};
use crate::components::settings::editor_state::{EditorSnapshot, EditorState};
use crate::components::settings::settings_navigation::{
    SettingDescriptor, SettingsSectionId, SettingsSubsectionId,
};
use crate::components::settings::settings_page::SettingsPage;
use crate::components::settings::widgets::{
    SelectOption, SettingsMetrics, SettingsPageUi, SettingsSelectState,
    action_button as render_button, button_keyed as settings_button_keyed, labelled_toggle,
    quiet_action_button, select_buttons, setting_reset, setting_row,
    setting_row_with_control_width, setting_slider_row, setting_target, settings_defaults,
    settings_disclosure, settings_disclosure_with_layout, settings_editor_row, settings_group,
    settings_group_contents, settings_select, settings_subtopic, settings_summary_disclosure,
    slider,
};
use crate::components::ui::input::{Escape, Input, InputEvent, InputState};
use crate::components::ui::menu::menu_panel;
use crate::components::ui::{ButtonExt, button_hover};
use crate::theme::colors;
use crate::theme::tokens::{Radii, RadiiPixels};
use gpui::prelude::FluentBuilder;
use gpui::*;
use std::{
    cell::RefCell,
    collections::HashMap,
    sync::{LazyLock, Mutex, MutexGuard},
};

const ACCOUNT_SIGN_IN: SettingDescriptor = SettingDescriptor {
    id: "account.sign-in",
    label: "Twitch account",
    aliases: &[
        "login",
        "logout",
        "sign in",
        "sign out",
        "anonymous",
        "reauthenticate",
        "switch account",
    ],
    control_id: "account-sign-in",
    disclosures: &[],
};

/// Account (page leaf).
pub const ACCOUNT_SETTINGS: &[SettingDescriptor] = &[ACCOUNT_SIGN_IN];

const SETTINGS_AS_POPOUT: SettingDescriptor = SettingDescriptor {
    id: "appearance.settings-window",
    label: "Open settings in standalone window",
    aliases: &["popout", "separate window", "detach"],
    control_id: "toggle-settings-window",
    disclosures: &[],
};

const MINIMIZE_TO_TRAY: SettingDescriptor = SettingDescriptor {
    id: "general.minimize-to-tray",
    label: "Minimize to tray",
    aliases: &["notification area", "system tray", "taskbar", "hide window"],
    control_id: "toggle-minimize-to-tray",
    disclosures: &[],
};

const CLOSE_TO_TRAY: SettingDescriptor = SettingDescriptor {
    id: "general.close-to-tray",
    label: "Close to tray",
    aliases: &[
        "notification area",
        "system tray",
        "taskbar",
        "alt f4",
        "hide window",
    ],
    control_id: "toggle-close-to-tray",
    disclosures: &[],
};

const WINDOW_TRANSPARENT: SettingDescriptor = SettingDescriptor {
    id: "appearance.window-transparent",
    label: "Transparent window",
    aliases: &["compositor", "translucent", "blur"],
    control_id: "toggle-general-window-transparent",
    disclosures: &["appearance-more-windows"],
};

const ALWAYS_ON_TOP: SettingDescriptor = SettingDescriptor {
    id: "appearance.always-on-top",
    label: "Always on top",
    aliases: &["pin", "stay on top", "z-order"],
    control_id: "toggle-general-always-on-top",
    disclosures: &["appearance-more-windows"],
};

const OVERLAY_CLICK_THROUGH: SettingDescriptor = SettingDescriptor {
    id: "appearance.overlay-click-through",
    label: "Click-through",
    aliases: &["overlay", "mouse", "pass through"],
    control_id: "toggle-general-overlay-click-through",
    disclosures: &["appearance-more-windows"],
};

const OVERLAY_GHOST_MODE: SettingDescriptor = SettingDescriptor {
    id: "appearance.overlay-ghost-mode",
    label: "Ghost mode",
    aliases: &["overlay", "header", "backdrop"],
    control_id: "toggle-general-overlay-ghost-mode",
    disclosures: &["appearance-more-windows"],
};

const OVERLAY_MAX_MESSAGES: SettingDescriptor = SettingDescriptor {
    id: "appearance.overlay-max-messages",
    label: "Visible messages",
    aliases: &["overlay", "message count", "backlog"],
    control_id: "overlay-max-messages",
    disclosures: &["appearance-more-windows"],
};

const OVERLAY_IDLE_FADE: SettingDescriptor = SettingDescriptor {
    id: "appearance.overlay-idle-fade",
    label: "Fade after idle",
    aliases: &["overlay", "idle", "timeout", "fade"],
    control_id: "overlay-idle-fade",
    disclosures: &["appearance-more-windows"],
};

const OVERLAY_MIN_OPACITY: SettingDescriptor = SettingDescriptor {
    id: "appearance.overlay-min-opacity",
    label: "Minimum opacity",
    aliases: &["overlay", "opacity", "transparency"],
    control_id: "overlay-min-opacity",
    disclosures: &["appearance-more-windows"],
};

const OVERLAY_SAVED_POSITION: SettingDescriptor = SettingDescriptor {
    id: "appearance.overlay-saved-position",
    label: "Saved position",
    aliases: &["overlay", "reset position", "placement"],
    control_id: "overlay-reset-position",
    disclosures: &["appearance-more-windows"],
};

/// General › Window & startup.
pub const GENERAL_SETTINGS_WINDOW_SETTINGS: &[SettingDescriptor] = &[
    SETTINGS_AS_POPOUT,
    #[cfg(target_os = "windows")]
    MINIMIZE_TO_TRAY,
    #[cfg(target_os = "windows")]
    CLOSE_TO_TRAY,
];

/// Appearance › Windows & overlay.
pub const APPEARANCE_WINDOWS_OVERLAY_SETTINGS: &[SettingDescriptor] = &[
    WINDOW_TRANSPARENT,
    ALWAYS_ON_TOP,
    OVERLAY_CLICK_THROUGH,
    OVERLAY_GHOST_MODE,
    OVERLAY_MAX_MESSAGES,
    OVERLAY_IDLE_FADE,
    OVERLAY_MIN_OPACITY,
    OVERLAY_SAVED_POSITION,
];

const UNIFIED_INPUT: SettingDescriptor = SettingDescriptor {
    id: "chat.unified-input",
    label: "Unified input mode",
    aliases: &["single input", "split", "focused split"],
    control_id: "toggle-general-unified-input",
    disclosures: &[],
};

const SHOW_SEND_BUTTON: SettingDescriptor = SettingDescriptor {
    id: "chat.show-send-button",
    label: "Show send button",
    aliases: &["send", "enter", "button"],
    control_id: "toggle-general-show-send",
    disclosures: &[],
};

const TAB_COMPLETE_USERNAMES: SettingDescriptor = SettingDescriptor {
    id: "chat.tab-complete-usernames",
    label: "Tab-complete recent chatters",
    aliases: &["tab completion", "username", "autocomplete"],
    control_id: "toggle-general-tab-complete-usernames",
    disclosures: &["chat-more-composing"],
};

const MENTION_COMMA: SettingDescriptor = SettingDescriptor {
    id: "chat.mention-comma",
    label: "Comma after first-word mentions",
    aliases: &["comma", "mention", "completion"],
    control_id: "toggle-general-completion-comma",
    disclosures: &["chat-more-composing"],
};

const CHARACTER_COUNTER: SettingDescriptor = SettingDescriptor {
    id: "chat.character-counter",
    label: "Character Counter",
    aliases: &["counter", "500", "limit"],
    control_id: "toggle-general-character-counter",
    disclosures: &["chat-more-composing"],
};

/// Chat › Input & composing.
pub const CHAT_INPUT_COMPOSING_SETTINGS: &[SettingDescriptor] = &[
    UNIFIED_INPUT,
    SHOW_SEND_BUTTON,
    TAB_COMPLETE_USERNAMES,
    MENTION_COMMA,
    CHARACTER_COUNTER,
];

const COLLAPSE_ASCII_ART: SettingDescriptor = SettingDescriptor {
    id: "chat.collapse-ascii-art",
    label: "Collapse ASCII art",
    aliases: &["ascii", "art", "collapse", "tall message"],
    control_id: "toggle-general-collapse-ascii-art",
    disclosures: &["chat-more-reading"],
};

const USERNAME_RIGHT_CLICK: SettingDescriptor = SettingDescriptor {
    id: "chat.username-right-click",
    label: "Username right click",
    aliases: &["right click", "username", "action"],
    control_id: "username-right-click-behavior",
    disclosures: &["chat-more-reading"],
};

const USERNAME_RIGHT_CLICK_MODIFIER_ACTION: SettingDescriptor = SettingDescriptor {
    id: "chat.username-right-click-modifier-action",
    label: "Right-click with modifier",
    aliases: &[
        "alternate right click",
        "modifier action",
        "reply",
        "mention",
        "ignore",
    ],
    control_id: "username-right-click-modifier-behavior",
    disclosures: &["chat-more-reading"],
};

const USERNAME_RIGHT_CLICK_MODIFIER: SettingDescriptor = SettingDescriptor {
    id: "chat.username-right-click-modifier",
    label: "Modifier + right click",
    aliases: &["modifier", "alt", "ctrl", "shift", "alternate click"],
    control_id: "username-right-click-modifier",
    disclosures: &["chat-more-reading"],
};

const SHOW_REPLY_BUTTON: SettingDescriptor = SettingDescriptor {
    id: "chat.show-reply-button",
    label: "Show reply button",
    aliases: &["reply", "thread", "hover button"],
    control_id: "toggle-general-show-reply-button",
    disclosures: &[],
};

const CHAT_PAUSE_MODIFIER: SettingDescriptor = SettingDescriptor {
    id: "chat.pause-modifier",
    label: "Pause modifier",
    aliases: &["pause", "modifier", "scroll lock"],
    control_id: "chat-pause-modifier",
    disclosures: &["chat-more-reading"],
};

const CHAT_HOVER_PAUSE: SettingDescriptor = SettingDescriptor {
    id: "chat.hover-pause",
    label: "Pause on hover",
    aliases: &["hover", "pause", "scroll"],
    control_id: "chat-hover-pause",
    disclosures: &[],
};

const PAUSE_INDICATOR: SettingDescriptor = SettingDescriptor {
    id: "chat.pause-indicator",
    label: "Show pause indicator",
    aliases: &["paused", "banner", "indicator"],
    control_id: "toggle-general-pause-indicator",
    disclosures: &["chat-more-reading"],
};

const LAST_READ_INDICATOR: SettingDescriptor = SettingDescriptor {
    id: "chat.last-read-indicator",
    label: "Show last read message indicator",
    aliases: &["last read", "last message", "unread", "divider", "marker"],
    control_id: "toggle-general-last-read-indicator",
    disclosures: &[],
};

const MOUSE_SCROLL_SPEED: SettingDescriptor = SettingDescriptor {
    id: "chat.mouse-scroll-speed",
    label: "Mouse scroll speed",
    aliases: &["scroll", "wheel", "scroll multiplier", "scroll speed"],
    control_id: "mouse-scroll-speed",
    disclosures: &["chat-more-reading"],
};

const COMPACT_SPLIT_HEADER: SettingDescriptor = SettingDescriptor {
    id: "chat.compact-split-header",
    label: "One-line split headers",
    aliases: &["split header", "compact", "one line"],
    control_id: "toggle-general-compact-split-header",
    disclosures: &["chat-more-reading"],
};

/// Chat › Message behavior.
pub const CHAT_MESSAGE_BEHAVIOR_SETTINGS: &[SettingDescriptor] = &[
    COLLAPSE_ASCII_ART,
    USERNAME_RIGHT_CLICK,
    USERNAME_RIGHT_CLICK_MODIFIER_ACTION,
    USERNAME_RIGHT_CLICK_MODIFIER,
    SHOW_REPLY_BUTTON,
    CHAT_PAUSE_MODIFIER,
    CHAT_HOVER_PAUSE,
    PAUSE_INDICATOR,
    LAST_READ_INDICATOR,
    MOUSE_SCROLL_SPEED,
    COMPACT_SPLIT_HEADER,
];

const SHOW_TIMESTAMPS: SettingDescriptor = SettingDescriptor {
    id: "chat.show-timestamps",
    label: "Message timestamps",
    aliases: &["timestamp", "clock", "time"],
    control_id: "toggle-general-message-timestamps",
    disclosures: &[],
};

const TIMESTAMP_FORMAT: SettingDescriptor = SettingDescriptor {
    id: "chat.timestamp-format",
    label: "Timestamp format",
    aliases: &["12 hour", "24 hour", "seconds", "pattern", "builder"],
    control_id: "timestamp-builder",
    disclosures: &["chat-more-reading", "chat-timestamp-format"],
};

const HIDE_TIMESTAMPS_WHEN_LIVE: SettingDescriptor = SettingDescriptor {
    id: "chat.hide-timestamps-when-live",
    label: "Hide timestamps while live",
    aliases: &["live", "hide", "timestamp"],
    control_id: "toggle-general-hide-live-timestamps",
    disclosures: &["chat-more-reading"],
};

/// Chat › Timestamps.
pub const CHAT_TIMESTAMPS_SETTINGS: &[SettingDescriptor] =
    &[SHOW_TIMESTAMPS, TIMESTAMP_FORMAT, HIDE_TIMESTAMPS_WHEN_LIVE];

/// Chat rows that stay visible on first open.
pub const CHAT_READING_SETTINGS: &[SettingDescriptor] = &[
    SHOW_TIMESTAMPS,
    CHAT_HOVER_PAUSE,
    SHOW_REPLY_BUTTON,
    LAST_READ_INDICATOR,
];

/// Chat composing rows that stay visible on first open.
pub const CHAT_COMPOSING_PRIMARY_SETTINGS: &[SettingDescriptor] =
    &[UNIFIED_INPUT, SHOW_SEND_BUTTON];

/// Secondary Reading controls, grouped in its one disclosure.
pub const CHAT_READING_MORE_SETTINGS: &[SettingDescriptor] = &[
    CHAT_PAUSE_MODIFIER,
    PAUSE_INDICATOR,
    MOUSE_SCROLL_SPEED,
    COLLAPSE_ASCII_ART,
    COMPACT_SPLIT_HEADER,
    USERNAME_RIGHT_CLICK,
    USERNAME_RIGHT_CLICK_MODIFIER_ACTION,
    USERNAME_RIGHT_CLICK_MODIFIER,
    TIMESTAMP_FORMAT,
    HIDE_TIMESTAMPS_WHEN_LIVE,
];

/// Secondary Composing controls, grouped in its one disclosure.
pub const CHAT_COMPOSING_MORE_SETTINGS: &[SettingDescriptor] =
    &[TAB_COMPLETE_USERNAMES, MENTION_COMMA, CHARACTER_COUNTER];

const OPEN_LINKS_INCOGNITO: SettingDescriptor = SettingDescriptor {
    id: "links-media.open-links-incognito",
    label: "Open links in incognito",
    aliases: &["private window", "incognito", "browser"],
    control_id: "toggle-general-incognito-links",
    disclosures: &["links-more"],
};

const ASK_BEFORE_OPENING_LINKS: SettingDescriptor = SettingDescriptor {
    id: "links-media.ask-before-opening-links",
    label: "Ask before opening links",
    aliases: &["confirmation", "trusted domain", "prompt"],
    control_id: "toggle-general-link-confirmation",
    disclosures: &[],
};

const BROWSER_PATH: SettingDescriptor = SettingDescriptor {
    id: "links-media.browser-path",
    label: "Private-window browser",
    aliases: &[
        "browser path",
        "executable",
        "custom browser",
        "default browser",
        "incognito browser",
    ],
    control_id: "settings-browser-path-field",
    disclosures: &["links-more"],
};

/// Links & Media › Links.
pub const LINKS_MEDIA_LINKS_SETTINGS: &[SettingDescriptor] =
    &[OPEN_LINKS_INCOGNITO, ASK_BEFORE_OPENING_LINKS, BROWSER_PATH];

const USE_CUSTOM_PLAYER: SettingDescriptor = SettingDescriptor {
    id: "links-media.use-custom-player",
    label: "Use a custom player",
    aliases: &["mpv", "vlc", "external player"],
    control_id: "toggle-general-custom-player",
    disclosures: &["players-editor"],
};

const CUSTOM_PLAYER_PATH: SettingDescriptor = SettingDescriptor {
    id: "links-media.custom-player-path",
    label: "Custom player path",
    aliases: &["mpv", "vlc", "executable"],
    control_id: "settings-custom-player-path-field",
    disclosures: &["players-editor"],
};

const STREAMLINK_PATH: SettingDescriptor = SettingDescriptor {
    id: "links-media.streamlink-path",
    label: "Streamlink path",
    aliases: &["streamlink", "executable", "cli"],
    control_id: "settings-streamlink-path-field",
    disclosures: &["players-editor"],
};

const STREAMLINK_QUALITY: SettingDescriptor = SettingDescriptor {
    id: "links-media.streamlink-quality",
    label: "Streamlink quality",
    aliases: &["best", "720p", "source", "quality"],
    control_id: "settings-streamlink-quality",
    disclosures: &["players-editor"],
};

const STREAMLINK_CUSTOM_QUALITY: SettingDescriptor = SettingDescriptor {
    id: "links-media.streamlink-custom-quality",
    label: "Custom quality",
    aliases: &["quality string", "streamlink"],
    control_id: "settings-streamlink-custom-quality-field",
    disclosures: &["players-editor"],
};

const STREAMLINK_EXTRA_ARGS: SettingDescriptor = SettingDescriptor {
    id: "links-media.streamlink-extra-args",
    label: "Extra Streamlink arguments",
    aliases: &["arguments", "flags", "cli"],
    control_id: "settings-streamlink-extra-args-field",
    disclosures: &["players-editor"],
};

/// Links & Media › Players.
pub const LINKS_MEDIA_PLAYERS_SETTINGS: &[SettingDescriptor] = &[
    USE_CUSTOM_PLAYER,
    CUSTOM_PLAYER_PATH,
    STREAMLINK_PATH,
    STREAMLINK_QUALITY,
    STREAMLINK_CUSTOM_QUALITY,
    STREAMLINK_EXTRA_ARGS,
];

const SETTINGS_FILE: SettingDescriptor = SettingDescriptor {
    id: "developer-tools.settings-file",
    label: "Settings file",
    aliases: &["settings.json", "path", "open", "copy"],
    control_id: "configuration-settings-file",
    disclosures: &["advanced-configuration-files"],
};

const SETTINGS_IMPORT: SettingDescriptor = SettingDescriptor {
    id: "developer-tools.settings-import",
    label: "Import settings JSON",
    aliases: &["import", "restore", "paste", "json"],
    control_id: "settings-import-json-field",
    disclosures: &["advanced-settings-import"],
};

const SETTINGS_EXPORT: SettingDescriptor = SettingDescriptor {
    id: "developer-tools.settings-export",
    label: "Export settings JSON",
    aliases: &["export", "backup", "copy", "json"],
    control_id: "configuration-export",
    disclosures: &[],
};

/// Developer Tools › Configuration.
pub const DEVELOPER_CONFIGURATION_SETTINGS: &[SettingDescriptor] = &[
    SETTINGS_FILE,
    SETTINGS_IMPORT,
    SETTINGS_EXPORT,
    super::chatterino_import::CHATTERINO_IMPORT,
    super::chatterino_import::CHATTERINO_IMPORT_FOLDER,
    super::chatterino_import::CHATTERINO_IMPORT_GROUPS,
    super::chatterino_import::CHATTERINO_IMPORT_TAB_MODE,
    super::chatterino_import::CHATTERINO_IMPORT_RUN,
];

pub const ADVANCED_DEVELOPER_MODE_SETTINGS: &[SettingDescriptor] = &[SettingDescriptor {
    id: "advanced.developer-mode",
    label: "Developer mode",
    aliases: &["developer mode", "diagnostics access"],
    control_id: "settings-diagnostics-toggle",
    disclosures: &[],
}];

#[derive(Clone, Copy, PartialEq, Eq)]
enum GeneralSettingsField {
    BrowserPath,
    CustomPlayerPath,
    StreamlinkPath,
    StreamlinkCustomQuality,
    StreamlinkExtraArgs,
}

#[derive(Clone, Default)]
struct GeneralSettingsEditor {
    browser_path_mode: bool,
    browser_path: String,
    browser_status: Option<String>,
    browser_status_is_error: bool,
    custom_player_path: String,
    custom_player_status: Option<String>,
    custom_player_status_is_error: bool,
    streamlink_path: String,
    streamlink_status: Option<String>,
    streamlink_status_is_error: bool,
    streamlink_custom_quality: String,
    streamlink_custom_quality_status: Option<String>,
    streamlink_custom_quality_status_is_error: bool,
    streamlink_extra_args: String,
    streamlink_extra_args_status: Option<String>,
    streamlink_extra_args_status_is_error: bool,
    focused: Option<GeneralSettingsField>,
    dirty: [bool; 5],
}

static GENERAL_SETTINGS_EDITOR: LazyLock<EditorState<GeneralSettingsEditor>> =
    LazyLock::new(|| EditorState::new("player paths", GeneralSettingsEditor::default()));

struct GeneralSettingsInputs {
    fields: [Option<Entity<InputState>>; 5],
    subscriptions: [Option<Subscription>; 5],
}

thread_local! {
    static GENERAL_SETTINGS_INPUTS: RefCell<HashMap<EntityId, GeneralSettingsInputs>> = RefCell::new(HashMap::new());
}

#[derive(Clone, Default)]
struct SettingsImportEditor {
    json: String,
    status: Option<String>,
    status_is_error: bool,
    focused: bool,
}

static SETTINGS_IMPORT_EDITOR: LazyLock<EditorState<SettingsImportEditor>> =
    LazyLock::new(|| EditorState::new("settings import", SettingsImportEditor::default()));

/// Render the Account page.
///
/// It is a leaf: Current Account and All Accounts are groups on one block, and
/// the tree must not grow or lose a node when the saved-account list changes.
///
/// When `login_modal_state` is `None` (e.g. in the standalone popout window),
/// login/logout buttons and account switching are shown as read-only.
pub fn render_account_section<T: 'static>(
    login_modal_state: Option<Entity<LoginModalState>>,
    ui: &mut SettingsPageUi,
    cx: &mut Context<T>,
) -> SettingsPage {
    SettingsPage::leaf(
        SettingsSectionId::General,
        render_account_body(login_modal_state, ui, cx),
    )
}

fn render_account_body<T: 'static>(
    login_modal_state: Option<Entity<LoginModalState>>,
    ui: &mut SettingsPageUi,
    cx: &mut Context<T>,
) -> impl IntoElement + use<T> {
    let auth_state = backend::get_auth_state();
    let is_authenticated = backend::is_authenticated();
    let anonymous_mode = backend::bridge::is_anonymous_mode();
    let accounts = backend::get_accounts();
    let active_account = accounts.iter().find(|account| account.is_active).cloned();

    let account_row = if is_authenticated || active_account.is_some() {
        render_authenticated_account(
            &auth_state,
            active_account.as_ref(),
            is_authenticated,
            anonymous_mode,
            login_modal_state.clone(),
            ui,
            cx,
        )
        .into_any_element()
    } else {
        match login_modal_state.clone() {
            Some(lms) => render_login_prompt(lms, ui, cx).into_any_element(),
            _ => render_login_prompt_readonly(ui, cx).into_any_element(),
        }
    };
    let account_count = accounts.len();
    let manage_accounts = login_modal_state.clone();
    let manage_disclosure = ui.disclosure("general-manage-accounts", cx);

    settings_group(
        "general-account",
        "Account",
        false,
        div()
            .w_full()
            .min_w_0()
            .flex()
            .flex_col()
            .child(account_row)
            .when(account_count > 0 && manage_accounts.is_some(), |content| {
                let login_modal_state = manage_accounts.expect("checked above");
                let summary = if account_count == 1 {
                    "1 saved account".to_string()
                } else {
                    format!("{account_count} saved accounts")
                };
                content.child(settings_summary_disclosure(
                    "general-manage-accounts",
                    "Manage accounts",
                    &summary,
                    &manage_disclosure,
                    cx,
                    move |cx| render_accounts_list(accounts, login_modal_state, cx),
                ))
            }),
    )
}

/// Render the Appearance page as its four scroll anchors.
pub fn render_appearance_section<T: 'static>(
    ui: &mut SettingsPageUi,
    cx: &mut Context<T>,
) -> SettingsPage {
    SettingsPage::new(SettingsSectionId::Theme)
        .subsection(
            SettingsSubsectionId::ThemeColors,
            super::render_appearance_theme_colors(ui, cx),
        )
        .subsection(
            SettingsSubsectionId::AppearanceMotion,
            super::render_appearance_motion(ui, cx),
        )
        .subsection(
            SettingsSubsectionId::TabsSizing,
            super::render_appearance_tabs_sizing(ui, cx),
        )
        .subsection(
            SettingsSubsectionId::AppearanceOptions,
            div()
                .w_full()
                .min_w_0()
                .flex()
                .flex_col()
                .gap_3()
                .child(super::render_appearance_more(ui, cx))
                .child(render_windows_overlay_block(ui, cx)),
        )
}

pub fn render_windows_overlay_block<T: 'static>(
    ui: &mut SettingsPageUi,
    cx: &mut Context<T>,
) -> impl IntoElement + use<T> {
    let (always_on_top, window_transparent, overlay_click_through, overlay_ghost_mode) = {
        use crate::backend::lock_utils::MutexExt;
        use crate::backend::settings::SETTINGS;
        let settings = SETTINGS.lock_or_recover();
        (
            settings.always_on_top,
            settings.window_transparent,
            settings.overlay_click_through,
            settings.overlay_ghost_mode,
        )
    };
    let overlay_max_messages = crate::backend::settings::get_overlay_max_messages();
    let overlay_idle_fade_secs = crate::backend::settings::get_overlay_idle_fade_secs();
    let overlay_min_opacity = crate::backend::settings::get_overlay_min_opacity();

    let defaults = settings_defaults();
    let modified = usize::from(always_on_top != defaults.always_on_top)
        + usize::from(window_transparent != defaults.window_transparent)
        + usize::from(overlay_click_through != defaults.overlay_click_through)
        + usize::from(overlay_ghost_mode != defaults.overlay_ghost_mode)
        + usize::from(overlay_max_messages != defaults.overlay_max_messages as usize)
        + usize::from(overlay_idle_fade_secs != defaults.overlay_idle_fade_secs as u64)
        + usize::from(
            (overlay_min_opacity * 100.0).round() as u8 != defaults.overlay_min_opacity_percent,
        );
    let stacked = ui.stacked;
    let disclosure = ui.disclosure("appearance-more-windows", cx);
    let max_messages_select = ui.select("overlay-max-messages", cx);
    let idle_fade_select = ui.select("overlay-idle-fade", cx);
    let min_opacity_select = ui.select("overlay-min-opacity", cx);

    settings_group(
        "appearance-window-overlay",
        "More window and overlay options",
        false,
        settings_disclosure(
            "appearance-more-windows",
            "More window and overlay options",
            Some("Transparency, placement, and overlay behavior"),
            modified,
            &disclosure,
            cx,
            move |cx| {
                div()
                    .w_full()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(px(SettingsMetrics::GROUP_GAP))
                    .child(settings_group_contents(
                        "appearance-window-options",
                        "Window",
                        true,
                        render_window_settings(always_on_top, window_transparent, stacked, cx),
                    ))
                    .child(settings_group_contents(
                        "appearance-overlay-options",
                        "Overlay",
                        true,
                        render_overlay_settings(
                            overlay_click_through,
                            overlay_ghost_mode,
                            overlay_max_messages,
                            overlay_idle_fade_secs,
                            overlay_min_opacity,
                            stacked,
                            &max_messages_select,
                            &idle_fade_select,
                            &min_opacity_select,
                            cx,
                        ),
                    ))
            },
        ),
    )
}

/// Primary Chat reading controls. The outer page owns the shared Reading surface.
pub fn render_chat_reading<T: 'static>(
    ui: &mut SettingsPageUi,
    cx: &mut Context<T>,
) -> impl IntoElement + use<T> {
    let defaults = settings_defaults();
    let show_timestamps = crate::backend::settings::get_show_message_timestamps();
    let hover_pause = crate::backend::settings::get_chat_hover_pause_mode();
    let show_reply_button = crate::backend::settings::get_show_reply_button();
    let show_last_read = crate::backend::settings::get_show_last_read_indicator();
    let hover_pause_select = ui.select(CHAT_HOVER_PAUSE.control_id, cx);
    let hover_pause_key = match hover_pause {
        ChatHoverPauseMode::Disabled => "off",
        ChatHoverPauseMode::Seconds2 => "2s",
        ChatHoverPauseMode::Seconds3 => "3s",
        ChatHoverPauseMode::Seconds5 => "5s",
        ChatHoverPauseMode::Seconds10 => "10s",
        ChatHoverPauseMode::Indefinite => "indefinite",
    };

    div()
        .w_full()
        .min_w_0()
        .flex()
        .flex_col()
        .gap(px(SettingsMetrics::GROUP_GAP))
        .child(setting_target(
            SHOW_TIMESTAMPS,
            crate::components::settings::widgets::setting_compact_row(
                SHOW_TIMESTAMPS.id,
                SHOW_TIMESTAMPS.label,
                Some("Use the operating system's local time for chat messages."),
                labelled_toggle(
                    SHOW_TIMESTAMPS.control_id,
                    SHOW_TIMESTAMPS.label,
                    show_timestamps,
                    cx,
                    |enabled, cx| {
                        crate::backend::settings::set_show_message_timestamps(enabled);
                        cx.notify();
                    },
                ),
                show_timestamps != defaults.show_message_timestamps,
                (show_timestamps != defaults.show_message_timestamps).then(|| {
                    let value = defaults.show_message_timestamps;
                    setting_reset(
                        "reset-show-message-timestamps",
                        SHOW_TIMESTAMPS.label,
                        cx,
                        move |cx| {
                            crate::backend::settings::set_show_message_timestamps(value);
                            cx.notify();
                        },
                    )
                }),
                ui.stacked,
            ),
        ))
        .child(setting_target(
            CHAT_HOVER_PAUSE,
            setting_row(
                CHAT_HOVER_PAUSE.id,
                CHAT_HOVER_PAUSE.label,
                Some(
                    "Pause chat while the pointer is over it, or for a short time after movement.",
                ),
                settings_select(
                    CHAT_HOVER_PAUSE.control_id,
                    CHAT_HOVER_PAUSE.label,
                    &[
                        SelectOption::new("off", "Off"),
                        SelectOption::new("2s", "2 seconds"),
                        SelectOption::new("3s", "3 seconds"),
                        SelectOption::new("5s", "5 seconds"),
                        SelectOption::new("10s", "10 seconds"),
                        SelectOption::new("indefinite", "While hovering"),
                    ],
                    hover_pause_key,
                    &hover_pause_select,
                    cx,
                    |value, cx| {
                        let mode = match value.as_str() {
                            "2s" => ChatHoverPauseMode::Seconds2,
                            "3s" => ChatHoverPauseMode::Seconds3,
                            "5s" => ChatHoverPauseMode::Seconds5,
                            "10s" => ChatHoverPauseMode::Seconds10,
                            "indefinite" => ChatHoverPauseMode::Indefinite,
                            _ => ChatHoverPauseMode::Disabled,
                        };
                        crate::backend::settings::set_chat_hover_pause_mode(mode);
                        cx.notify();
                    },
                ),
                hover_pause != defaults.chat_hover_pause_mode,
                (hover_pause != defaults.chat_hover_pause_mode).then(|| {
                    let value = defaults.chat_hover_pause_mode;
                    setting_reset(
                        "reset-chat-hover-pause",
                        CHAT_HOVER_PAUSE.label,
                        cx,
                        move |cx| {
                            crate::backend::settings::set_chat_hover_pause_mode(value);
                            cx.notify();
                        },
                    )
                }),
                ui.stacked,
            ),
        ))
        .child(setting_target(
            SHOW_REPLY_BUTTON,
            crate::components::settings::widgets::setting_compact_row(
                SHOW_REPLY_BUTTON.id,
                SHOW_REPLY_BUTTON.label,
                Some("Show a reply button next to every visible chat message."),
                labelled_toggle(
                    SHOW_REPLY_BUTTON.control_id,
                    SHOW_REPLY_BUTTON.label,
                    show_reply_button,
                    cx,
                    |enabled, cx| {
                        crate::backend::settings::set_show_reply_button(enabled);
                        cx.notify();
                    },
                ),
                show_reply_button != defaults.show_reply_button,
                (show_reply_button != defaults.show_reply_button).then(|| {
                    let value = defaults.show_reply_button;
                    setting_reset(
                        "reset-show-reply-button",
                        SHOW_REPLY_BUTTON.label,
                        cx,
                        move |cx| {
                            crate::backend::settings::set_show_reply_button(value);
                            cx.notify();
                        },
                    )
                }),
                ui.stacked,
            ),
        ))
        .child(setting_target(
            LAST_READ_INDICATOR,
            crate::components::settings::widgets::setting_compact_row(
                LAST_READ_INDICATOR.id,
                LAST_READ_INDICATOR.label,
                Some("Draw a line under the last message seen before leaving a tab or window."),
                labelled_toggle(
                    LAST_READ_INDICATOR.control_id,
                    LAST_READ_INDICATOR.label,
                    show_last_read,
                    cx,
                    |enabled, cx| {
                        crate::backend::settings::set_show_last_read_indicator(enabled);
                        cx.notify();
                    },
                ),
                show_last_read != defaults.show_last_read_indicator,
                (show_last_read != defaults.show_last_read_indicator).then(|| {
                    let value = defaults.show_last_read_indicator;
                    setting_reset(
                        "reset-last-read-indicator",
                        LAST_READ_INDICATOR.label,
                        cx,
                        move |cx| {
                            crate::backend::settings::set_show_last_read_indicator(value);
                            cx.notify();
                        },
                    )
                }),
                ui.stacked,
            ),
        ))
}

/// Primary Chat composing controls. The outer page owns the shared Composing surface.
pub fn render_chat_composing<T: 'static>(
    ui: &SettingsPageUi,
    cx: &mut Context<T>,
) -> impl IntoElement + use<T> {
    let defaults = settings_defaults();
    let unified_mode = crate::backend::settings::get_unified_input_mode();
    let show_send_button = crate::backend::settings::get_show_send_button();

    div()
        .w_full()
        .min_w_0()
        .flex()
        .flex_col()
        .gap(px(SettingsMetrics::GROUP_GAP))
        .child(setting_target(
            UNIFIED_INPUT,
            crate::components::settings::widgets::setting_compact_row(
                UNIFIED_INPUT.id,
                UNIFIED_INPUT.label,
                Some("Send from one input that targets the focused split."),
                labelled_toggle(
                    UNIFIED_INPUT.control_id,
                    UNIFIED_INPUT.label,
                    unified_mode,
                    cx,
                    |enabled, cx| {
                        crate::backend::settings::set_unified_input_mode(enabled);
                        cx.notify();
                    },
                ),
                unified_mode != defaults.unified_input_mode,
                (unified_mode != defaults.unified_input_mode).then(|| {
                    let value = defaults.unified_input_mode;
                    setting_reset(
                        "reset-unified-input-mode",
                        UNIFIED_INPUT.label,
                        cx,
                        move |cx| {
                            crate::backend::settings::set_unified_input_mode(value);
                            cx.notify();
                        },
                    )
                }),
                ui.stacked,
            ),
        ))
        .child(setting_target(
            SHOW_SEND_BUTTON,
            crate::components::settings::widgets::setting_compact_row(
                SHOW_SEND_BUTTON.id,
                SHOW_SEND_BUTTON.label,
                Some("Display a send button; Enter always sends."),
                labelled_toggle(
                    SHOW_SEND_BUTTON.control_id,
                    SHOW_SEND_BUTTON.label,
                    show_send_button,
                    cx,
                    |enabled, cx| {
                        crate::backend::settings::set_show_send_button(enabled);
                        cx.notify();
                    },
                ),
                show_send_button != defaults.show_send_button,
                (show_send_button != defaults.show_send_button).then(|| {
                    let value = defaults.show_send_button;
                    setting_reset(
                        "reset-show-send-button",
                        SHOW_SEND_BUTTON.label,
                        cx,
                        move |cx| {
                            crate::backend::settings::set_show_send_button(value);
                            cx.notify();
                        },
                    )
                }),
                ui.stacked,
            ),
        ))
}

/// Secondary Reading controls. The enclosing Reading group owns this disclosure.
pub fn render_chat_reading_more<T: 'static>(
    ui: &mut SettingsPageUi,
    window: &mut Window,
    cx: &mut Context<T>,
) -> impl IntoElement + use<T> {
    let defaults = settings_defaults();
    let stacked = ui.stacked;
    let pause_modifier = crate::backend::settings::get_chat_pause_modifier();
    let show_pause_indicator = crate::backend::settings::get_show_pause_indicator();
    let mouse_scroll = crate::backend::settings::get_mouse_scroll_multiplier();
    let collapse_ascii = crate::backend::settings::get_collapse_ascii_art();
    let compact_header = crate::backend::settings::get_compact_split_header();
    let right_click = crate::backend::settings::get_username_right_click_behavior();
    let modifier_action = crate::backend::settings::get_username_right_click_modifier_behavior();
    let right_click_modifier = crate::backend::settings::get_username_right_click_modifier();
    let hide_live = crate::backend::settings::get_hide_message_timestamps_when_live();
    let pattern = crate::backend::settings::get_message_timestamp_pattern();
    let reading = ui.disclosure("chat-more-reading", cx);
    let timestamp_format = ui.disclosure("chat-timestamp-format", cx);
    let literal_input = timestamp_literal_input(&timestamp_builder_snapshot().literal_draft)
        .escape_focus(timestamp_format.clone())
        .prepare(window, cx);
    let timestamp_literal_preset_select = ui.select("timestamp-literal-preset", cx);
    let pause_modifier_select = ui.select(CHAT_PAUSE_MODIFIER.control_id, cx);
    let right_click_select = ui.select(USERNAME_RIGHT_CLICK.control_id, cx);
    let modifier_action_select = ui.select(USERNAME_RIGHT_CLICK_MODIFIER_ACTION.control_id, cx);
    let right_click_modifier_select = ui.select(USERNAME_RIGHT_CLICK_MODIFIER.control_id, cx);
    let timestamp_view = timestamp_builder_snapshot();
    let timestamp_summary = pattern
        .tokens
        .iter()
        .map(TimestampToken::accessible_name)
        .collect::<Vec<_>>()
        .join(" · ");
    let timestamp_summary = if !timestamp_view.literal_draft.is_empty() {
        format!("{timestamp_summary} · Unsaved draft")
    } else if timestamp_view.literal_focused || timestamp_view.selected.is_some() {
        format!("{timestamp_summary} · Editing…")
    } else {
        timestamp_summary
    };
    let modifier_key = match pause_modifier {
        ChatPauseModifier::Disabled => "off",
        ChatPauseModifier::Alt => "alt",
        ChatPauseModifier::Control => "control",
        ChatPauseModifier::Shift => "shift",
    };
    let platform_label = if cfg!(target_os = "macos") {
        "Cmd"
    } else if cfg!(target_os = "windows") {
        "Win"
    } else {
        "Super"
    };
    let scrolling_modified = usize::from(pause_modifier != defaults.chat_pause_modifier)
        + usize::from(show_pause_indicator != defaults.show_pause_indicator)
        + usize::from(
            crate::components::settings::widgets::setting_float_modified(
                mouse_scroll,
                defaults.mouse_scroll_multiplier,
            ),
        );
    let message_modified = usize::from(collapse_ascii != defaults.collapse_ascii_art)
        + usize::from(compact_header != defaults.compact_split_header)
        + super::chat_message_display_modified_count();
    let interaction_modified = usize::from(right_click != defaults.username_right_click_behavior)
        + usize::from(modifier_action != defaults.username_right_click_modifier_behavior)
        + usize::from(right_click_modifier != defaults.username_right_click_modifier);
    let default_pattern = defaults
        .message_timestamp_pattern
        .clone()
        .unwrap_or_default();
    let timestamps_modified = usize::from(pattern != default_pattern)
        + usize::from(hide_live != defaults.hide_message_timestamps_when_live);

    let reading_modified = scrolling_modified
        + message_modified
        + interaction_modified
        + timestamps_modified
        + super::chat_names_modified_count();

    let reading_summary = if !timestamp_view.literal_draft.is_empty() {
        "Unsaved draft · Names, scrolling, message display, right-click, timestamps"
    } else if timestamp_view.literal_focused || timestamp_view.selected.is_some() {
        "Editing… · Names, scrolling, message display, right-click, timestamps"
    } else {
        "Names, scrolling, message display, right-click, timestamps"
    };

    settings_disclosure_with_layout(
        "chat-more-reading",
        "More reading settings",
        Some(reading_summary),
        reading_modified,
        &reading,
        stacked,
        cx,
        move |cx| {
            div()
            .w_full()
            .min_w_0()
            .flex()
            .flex_col()
            .gap(px(SettingsMetrics::GROUP_GAP))
            .child(settings_subtopic(
                "chat-reading-names",
                "Names",
                super::render_chat_name_options(stacked, cx),
            ))
            .child(settings_subtopic(
                "chat-reading-scrolling",
                "Scrolling",
                div()
                    .flex()
                    .flex_col()
                    .gap(px(SettingsMetrics::GROUP_GAP))
                        .child(setting_target(
                            CHAT_PAUSE_MODIFIER,
                            setting_row(
                                CHAT_PAUSE_MODIFIER.id,
                                CHAT_PAUSE_MODIFIER.label,
                                Some("Temporarily pause chat while the selected modifier is held."),
                                settings_select(
                                    CHAT_PAUSE_MODIFIER.control_id,
                                    CHAT_PAUSE_MODIFIER.label,
                                    &[
                                        SelectOption::new("off", "Off"),
                                        SelectOption::new("alt", "Alt"),
                                        SelectOption::new("control", "Ctrl"),
                                        SelectOption::new("shift", "Shift"),
                                    ],
                                    modifier_key,
                                    &pause_modifier_select,
                                    cx,
                                    |value, cx| {
                                        let modifier = match value.as_str() {
                                            "alt" => ChatPauseModifier::Alt,
                                            "control" => ChatPauseModifier::Control,
                                            "shift" => ChatPauseModifier::Shift,
                                            _ => ChatPauseModifier::Disabled,
                                        };
                                        crate::backend::settings::set_chat_pause_modifier(modifier);
                                        cx.notify();
                                    },
                                ),
                                pause_modifier != defaults.chat_pause_modifier,
                                (pause_modifier != defaults.chat_pause_modifier).then(|| {
                                    let value = defaults.chat_pause_modifier;
                                    setting_reset("reset-chat-pause-modifier", CHAT_PAUSE_MODIFIER.label, cx, move |cx| {
                                        crate::backend::settings::set_chat_pause_modifier(value);
                                        cx.notify();
                                    })
                                }),
                                stacked,
                            ),
                        ))
                        .child(setting_target(
                            PAUSE_INDICATOR,
                            crate::components::settings::widgets::setting_compact_row(
                                PAUSE_INDICATOR.id,
                                PAUSE_INDICATOR.label,
                                Some("Display a badge when chat is paused by the hold key or hover."),
                                labelled_toggle(PAUSE_INDICATOR.control_id, PAUSE_INDICATOR.label, show_pause_indicator, cx, |enabled, cx| {
                                    crate::backend::settings::set_show_pause_indicator(enabled);
                                    cx.notify();
                                }),
                                show_pause_indicator != defaults.show_pause_indicator,
                                (show_pause_indicator != defaults.show_pause_indicator).then(|| {
                                    let value = defaults.show_pause_indicator;
                                    setting_reset("reset-show-pause-indicator", PAUSE_INDICATOR.label, cx, move |cx| {
                                        crate::backend::settings::set_show_pause_indicator(value);
                                        cx.notify();
                                    })
                                }),
                                stacked,
                            ),
                        ))
                        .child(setting_target(
                            MOUSE_SCROLL_SPEED,
                            setting_slider_row(
                                MOUSE_SCROLL_SPEED.id,
                                MOUSE_SCROLL_SPEED.label,
                                Some("Scale how far chat moves per mouse-wheel notch. Touchpads are not affected."),
                                slider(
                                    MOUSE_SCROLL_SPEED.control_id,
                                    mouse_scroll,
                                    crate::backend::settings::MOUSE_SCROLL_MULTIPLIER_MIN,
                                    crate::backend::settings::MOUSE_SCROLL_MULTIPLIER_MAX,
                                    0.1,
                                    |value| format!("{value:.1}x"),
                                    cx,
                                    |value, cx| {
                                        crate::backend::settings::set_mouse_scroll_multiplier(value);
                                        cx.notify();
                                    },
                                ),
                                crate::components::settings::widgets::setting_float_modified(
                                    mouse_scroll,
                                    defaults.mouse_scroll_multiplier,
                                ),
                                crate::components::settings::widgets::setting_float_modified(
                                    mouse_scroll,
                                    defaults.mouse_scroll_multiplier,
                                )
                                .then(|| {
                                    let value = defaults.mouse_scroll_multiplier;
                                    setting_reset("reset-mouse-scroll-speed", MOUSE_SCROLL_SPEED.label, cx, move |cx| {
                                        crate::backend::settings::set_mouse_scroll_multiplier(value);
                                        cx.notify();
                                    })
                                }),
                                stacked,
                            ),
                        ))
            ))
            .child(settings_subtopic(
                "chat-reading-message-display",
                "Message display",
                div()
                    .flex()
                    .flex_col()
                    .gap(px(SettingsMetrics::GROUP_GAP))
                        .child(setting_target(
                            COLLAPSE_ASCII_ART,
                            crate::components::settings::widgets::setting_compact_row(
                                COLLAPSE_ASCII_ART.id,
                                COLLAPSE_ASCII_ART.label,
                                Some("Collapse multi-line ASCII art messages to a single line."),
                                labelled_toggle(COLLAPSE_ASCII_ART.control_id, COLLAPSE_ASCII_ART.label, collapse_ascii, cx, |enabled, cx| {
                                    crate::backend::settings::set_collapse_ascii_art(enabled);
                                    cx.notify();
                                }),
                                collapse_ascii != defaults.collapse_ascii_art,
                                (collapse_ascii != defaults.collapse_ascii_art).then(|| {
                                    let value = defaults.collapse_ascii_art;
                                    setting_reset("reset-collapse-ascii-art", COLLAPSE_ASCII_ART.label, cx, move |cx| {
                                        crate::backend::settings::set_collapse_ascii_art(value);
                                        cx.notify();
                                    })
                                }),
                                stacked,
                            ),
                        ))
                        .child(setting_target(
                            COMPACT_SPLIT_HEADER,
                            crate::components::settings::widgets::setting_compact_row(
                                COMPACT_SPLIT_HEADER.id,
                                COMPACT_SPLIT_HEADER.label,
                                Some("Put channel, stream status, category, and chat modes on one compact row."),
                                labelled_toggle(COMPACT_SPLIT_HEADER.control_id, COMPACT_SPLIT_HEADER.label, compact_header, cx, |enabled, cx| {
                                    crate::backend::settings::set_compact_split_header(enabled);
                                    cx.notify();
                                }),
                                compact_header != defaults.compact_split_header,
                                (compact_header != defaults.compact_split_header).then(|| {
                                    let value = defaults.compact_split_header;
                                    setting_reset("reset-compact-split-header", COMPACT_SPLIT_HEADER.label, cx, move |cx| {
                                        crate::backend::settings::set_compact_split_header(value);
                                        cx.notify();
                                    })
                                }),
                                stacked,
                            ),
                        ))
                        .child(super::render_chat_message_display_options(stacked, cx))
            ))
            .child(settings_subtopic(
                "chat-reading-right-click",
                "Right-click",
                {
                    let behavior_options = [
                        SelectOption::new("reply", "Reply"),
                        SelectOption::new("mention", "Mention"),
                        SelectOption::new("ignore", "Ignore"),
                    ];
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(SettingsMetrics::GROUP_GAP))
                        .child(setting_target(
                            USERNAME_RIGHT_CLICK,
                            setting_row(
                                USERNAME_RIGHT_CLICK.id,
                                USERNAME_RIGHT_CLICK.label,
                                Some("Choose the action used when no modifier is held."),
                                settings_select(USERNAME_RIGHT_CLICK.control_id, USERNAME_RIGHT_CLICK.label, &behavior_options, right_click.settings_value(), &right_click_select, cx, |value, cx| {
                                    let behavior = match value.as_str() {
                                        "reply" => UsernameRightClickBehavior::Reply,
                                        "ignore" => UsernameRightClickBehavior::Ignore,
                                        _ => UsernameRightClickBehavior::Mention,
                                    };
                                    crate::backend::settings::set_username_right_click_behavior(behavior);
                                    cx.notify();
                                }),
                                right_click != defaults.username_right_click_behavior,
                                (right_click != defaults.username_right_click_behavior).then(|| {
                                    let value = defaults.username_right_click_behavior;
                                    setting_reset("reset-username-right-click", USERNAME_RIGHT_CLICK.label, cx, move |cx| {
                                        crate::backend::settings::set_username_right_click_behavior(value);
                                        cx.notify();
                                    })
                                }),
                                stacked,
                            ),
                        ))
                        .child(setting_target(
                            USERNAME_RIGHT_CLICK_MODIFIER_ACTION,
                            setting_row(
                                USERNAME_RIGHT_CLICK_MODIFIER_ACTION.id,
                                USERNAME_RIGHT_CLICK_MODIFIER_ACTION.label,
                                Some("Choose the alternate action used with exactly the selected modifier."),
                                settings_select(USERNAME_RIGHT_CLICK_MODIFIER_ACTION.control_id, USERNAME_RIGHT_CLICK_MODIFIER_ACTION.label, &behavior_options, modifier_action.settings_value(), &modifier_action_select, cx, |value, cx| {
                                    let behavior = match value.as_str() {
                                        "mention" => UsernameRightClickBehavior::Mention,
                                        "ignore" => UsernameRightClickBehavior::Ignore,
                                        _ => UsernameRightClickBehavior::Reply,
                                    };
                                    crate::backend::settings::set_username_right_click_modifier_behavior(behavior);
                                    cx.notify();
                                }),
                                modifier_action != defaults.username_right_click_modifier_behavior,
                                (modifier_action != defaults.username_right_click_modifier_behavior).then(|| {
                                    let value = defaults.username_right_click_modifier_behavior;
                                    setting_reset("reset-username-right-click-modifier-action", USERNAME_RIGHT_CLICK_MODIFIER_ACTION.label, cx, move |cx| {
                                        crate::backend::settings::set_username_right_click_modifier_behavior(value);
                                        cx.notify();
                                    })
                                }),
                                stacked,
                            ),
                        ))
                        .child(setting_target(
                            USERNAME_RIGHT_CLICK_MODIFIER,
                            setting_row(
                                USERNAME_RIGHT_CLICK_MODIFIER.id,
                                USERNAME_RIGHT_CLICK_MODIFIER.label,
                                Some("Extra held modifiers intentionally fall back to the normal action."),
                                settings_select(USERNAME_RIGHT_CLICK_MODIFIER.control_id, USERNAME_RIGHT_CLICK_MODIFIER.label, &[
                                    SelectOption::new("shift", "Shift"),
                                    SelectOption::new("control", "Ctrl"),
                                    SelectOption::new("alt", "Alt"),
                                    SelectOption::new("platform", platform_label),
                                ], right_click_modifier.settings_value(), &right_click_modifier_select, cx, |value, cx| {
                                    let modifier = match value.as_str() {
                                        "control" => UsernameRightClickModifier::Control,
                                        "alt" => UsernameRightClickModifier::Alt,
                                        "platform" => UsernameRightClickModifier::Platform,
                                        _ => UsernameRightClickModifier::Shift,
                                    };
                                    crate::backend::settings::set_username_right_click_modifier(modifier);
                                    cx.notify();
                                }),
                                right_click_modifier != defaults.username_right_click_modifier,
                                (right_click_modifier != defaults.username_right_click_modifier).then(|| {
                                    let value = defaults.username_right_click_modifier;
                                    setting_reset("reset-username-right-click-modifier", USERNAME_RIGHT_CLICK_MODIFIER.label, cx, move |cx| {
                                        crate::backend::settings::set_username_right_click_modifier(value);
                                        cx.notify();
                                    })
                                }),
                                stacked,
                            ),
                        ))
                }
            ))
            .child(settings_subtopic(
                "chat-reading-timestamps",
                "Timestamps",
                div()
                    .flex()
                    .flex_col()
                    .gap(px(SettingsMetrics::GROUP_GAP))
                        .child(setting_target(
                            TIMESTAMP_FORMAT,
                            settings_editor_row(
                                "chat-timestamp-format",
                                "Timestamp format",
                                &timestamp_summary,
                                "Customize",
                                pattern != default_pattern,
                                (pattern != default_pattern).then(|| {
                                    let tokens = default_pattern.tokens.clone();
                                    setting_reset(
                                        "reset-timestamp-format",
                                        TIMESTAMP_FORMAT.label,
                                        cx,
                                        move |cx| {
                                            commit_timestamp_tokens(tokens.clone(), None, cx);
                                            cx.notify();
                                        },
                                    )
                                }),
                                &timestamp_format,
                                stacked,
                                cx,
                                move |cx| {
                                    div()
                                        .w_full()
                                        .min_w_0()
                                        .on_key_down(cx.listener(
                                            |_this, event: &KeyDownEvent, _window, cx| {
                                                if handle_timestamp_builder_key(event, cx) {
                                                    if event.keystroke.key == "escape" {
                                                        cx.stop_propagation();
                                                    }
                                                    cx.notify();
                                                }
                                            },
                                        ))
                                        .child(render_timestamp_builder(
                                            pattern,
                                            timestamp_literal_preset_select.clone(),
                                            literal_input,
                                            cx,
                                        ))
                                },
                            ),
                        ))
                        .child(setting_target(
                            HIDE_TIMESTAMPS_WHEN_LIVE,
                            crate::components::settings::widgets::setting_compact_row(
                                HIDE_TIMESTAMPS_WHEN_LIVE.id,
                                HIDE_TIMESTAMPS_WHEN_LIVE.label,
                                Some("Hide the timestamp column only while the channel is live."),
                                labelled_toggle(HIDE_TIMESTAMPS_WHEN_LIVE.control_id, HIDE_TIMESTAMPS_WHEN_LIVE.label, hide_live, cx, |enabled, cx| {
                                    crate::backend::settings::set_hide_message_timestamps_when_live(enabled);
                                    cx.notify();
                                }),
                                hide_live != defaults.hide_message_timestamps_when_live,
                                (hide_live != defaults.hide_message_timestamps_when_live).then(|| {
                                    let value = defaults.hide_message_timestamps_when_live;
                                    setting_reset("reset-hide-timestamps-when-live", HIDE_TIMESTAMPS_WHEN_LIVE.label, cx, move |cx| {
                                        crate::backend::settings::set_hide_message_timestamps_when_live(value);
                                        cx.notify();
                                    })
                                }),
                                stacked,
                            ),
                        ))
            ))
        },
    )
}

/// Secondary Composing controls. The enclosing Composing group owns this disclosure.
pub fn render_chat_composing_more<T: 'static>(
    ui: &mut SettingsPageUi,
    cx: &mut Context<T>,
) -> impl IntoElement + use<T> {
    let defaults = settings_defaults();
    let stacked = ui.stacked;
    let (tab_complete, mention_comma) = {
        use crate::backend::lock_utils::MutexExt;
        use crate::backend::settings::SETTINGS;

        let settings = SETTINGS.lock_or_recover();
        (
            settings.tab_complete_usernames,
            settings.mention_users_with_comma,
        )
    };
    let character_counter = crate::backend::settings::get_show_char_counter();
    let modified = usize::from(tab_complete != defaults.tab_complete_usernames)
        + usize::from(mention_comma != defaults.mention_users_with_comma)
        + usize::from(character_counter != defaults.show_char_counter);
    let disclosure = ui.disclosure("chat-more-composing", cx);

    settings_disclosure_with_layout(
        "chat-more-composing",
        "More composing settings",
        Some("Autocomplete, mentions, and character count"),
        modified,
        &disclosure,
        stacked,
        cx,
        move |cx| {
            div()
                .w_full()
                .min_w_0()
                .flex()
                .flex_col()
                .child(setting_target(
                    TAB_COMPLETE_USERNAMES,
                    crate::components::settings::widgets::setting_compact_row(
                        TAB_COMPLETE_USERNAMES.id,
                        TAB_COMPLETE_USERNAMES.label,
                        Some("Include recent usernames when cycling bare-token Tab matches."),
                        labelled_toggle(
                            TAB_COMPLETE_USERNAMES.control_id,
                            TAB_COMPLETE_USERNAMES.label,
                            tab_complete,
                            cx,
                            |enabled, cx| {
                                crate::backend::settings::set_tab_complete_usernames(enabled);
                                cx.notify();
                            },
                        ),
                        tab_complete != defaults.tab_complete_usernames,
                        (tab_complete != defaults.tab_complete_usernames).then(|| {
                            let value = defaults.tab_complete_usernames;
                            setting_reset(
                                "reset-tab-complete-usernames",
                                TAB_COMPLETE_USERNAMES.label,
                                cx,
                                move |cx| {
                                    crate::backend::settings::set_tab_complete_usernames(value);
                                    cx.notify();
                                },
                            )
                        }),
                        stacked,
                    ),
                ))
                .child(setting_target(
                    MENTION_COMMA,
                    crate::components::settings::widgets::setting_compact_row(
                        MENTION_COMMA.id,
                        MENTION_COMMA.label,
                        Some("Insert a comma when a completed username starts a message."),
                        labelled_toggle(
                            MENTION_COMMA.control_id,
                            MENTION_COMMA.label,
                            mention_comma,
                            cx,
                            |enabled, cx| {
                                crate::backend::settings::set_mention_users_with_comma(enabled);
                                cx.notify();
                            },
                        ),
                        mention_comma != defaults.mention_users_with_comma,
                        (mention_comma != defaults.mention_users_with_comma).then(|| {
                            let value = defaults.mention_users_with_comma;
                            setting_reset(
                                "reset-mention-comma",
                                MENTION_COMMA.label,
                                cx,
                                move |cx| {
                                    crate::backend::settings::set_mention_users_with_comma(value);
                                    cx.notify();
                                },
                            )
                        }),
                        stacked,
                    ),
                ))
                .child(setting_target(
                    CHARACTER_COUNTER,
                    crate::components::settings::widgets::setting_compact_row(
                        CHARACTER_COUNTER.id,
                        CHARACTER_COUNTER.label,
                        Some("Show the message length while typing."),
                        labelled_toggle(
                            CHARACTER_COUNTER.control_id,
                            CHARACTER_COUNTER.label,
                            character_counter,
                            cx,
                            |enabled, cx| {
                                crate::backend::settings::set_show_char_counter(enabled);
                                cx.notify();
                            },
                        ),
                        character_counter != defaults.show_char_counter,
                        (character_counter != defaults.show_char_counter).then(|| {
                            let value = defaults.show_char_counter;
                            setting_reset(
                                "reset-character-counter",
                                CHARACTER_COUNTER.label,
                                cx,
                                move |cx| {
                                    crate::backend::settings::set_show_char_counter(value);
                                    cx.notify();
                                },
                            )
                        }),
                        stacked,
                    ),
                ))
        },
    )
}

/// Render the Links & Media page as its four scroll anchors: image uploads,
/// link previews, link handling, and player integration. Network proxy controls
/// intentionally live behind Developer Tools so ordinary users do not have to
/// parse transport details.
pub fn render_connections_section<T: 'static>(
    uploader_inputs: &super::UploaderEditorInputs,
    ui: &mut SettingsPageUi,
    window: &mut Window,
    cx: &mut Context<T>,
) -> SettingsPage {
    let browser_path = crate::backend::settings::get_browser_path().unwrap_or_default();
    let custom_player_path = crate::backend::settings::get_custom_player_path().unwrap_or_default();
    let streamlink_path = crate::backend::settings::get_streamlink_path().unwrap_or_default();
    let streamlink_custom_quality = crate::backend::settings::get_streamlink_custom_quality();
    let streamlink_extra_args = crate::backend::settings::get_streamlink_extra_args();
    let editor = synced_general_settings_editor(
        &browser_path,
        &custom_player_path,
        &streamlink_path,
        &streamlink_custom_quality,
        &streamlink_extra_args,
    );
    if settings_import_editor_snapshot().draft.focused {
        update_settings_import_editor(|editor| editor.focused = false);
    }

    let links_more = ui.disclosure("links-more", cx);
    let players_editor = ui.disclosure("players-editor", cx);
    let browser_mode_select = ui.select("links-browser-mode", cx);
    let streamlink_quality_select = ui.select("players-streamlink-quality", cx);
    let browser_editor_visible = links_more.is_expanded()
        && (editor.draft.browser_path_mode
            || !editor.draft.browser_path.is_empty()
            || links_more.is_search_revealed());
    let streamlink_quality = crate::backend::settings::get_streamlink_quality();
    let mut visible_fields = Vec::new();
    if browser_editor_visible {
        visible_fields.push(GeneralSettingsField::BrowserPath);
    }
    if players_editor.is_expanded() {
        visible_fields.extend([
            GeneralSettingsField::CustomPlayerPath,
            GeneralSettingsField::StreamlinkPath,
            GeneralSettingsField::StreamlinkExtraArgs,
        ]);
        if streamlink_quality == StreamlinkQuality::Custom || players_editor.is_search_revealed() {
            visible_fields.push(GeneralSettingsField::StreamlinkCustomQuality);
        }
    }
    if visible_fields.is_empty() {
        release_general_settings_inputs_for_host(cx.entity_id());
    } else {
        ensure_general_settings_inputs(&editor.draft, &visible_fields, window, cx);
    }

    SettingsPage::new(SettingsSectionId::Connections)
        .subsection(
            SettingsSubsectionId::Links,
            render_links_surface(
                editor.clone(),
                &links_more,
                &browser_mode_select,
                ui.stacked,
                window,
                cx,
            ),
        )
        .subsection(
            SettingsSubsectionId::LinkPreviews,
            super::render_embeds_section(ui, cx),
        )
        .subsection(
            SettingsSubsectionId::ImageUploads,
            super::render_uploader_section(uploader_inputs, ui, window, cx),
        )
        .subsection(
            SettingsSubsectionId::Players,
            render_players_surface(
                editor,
                &players_editor,
                &streamlink_quality_select,
                ui.stacked,
                window,
                cx,
            ),
        )
}

/// The settings paths are fixed for the process, and resolving one builds a
/// `SettingsManager` (directory creation and existence checks), so the
/// Advanced page resolves them once instead of on every render.
static SETTINGS_FILE_PATH: LazyLock<std::path::PathBuf> =
    LazyLock::new(crate::backend::settings::settings_file_path);
static SETTINGS_DIR_PATH: LazyLock<std::path::PathBuf> =
    LazyLock::new(crate::backend::settings::settings_dir_path);
static SETTINGS_BACKUPS_DIR_PATH: LazyLock<std::path::PathBuf> =
    LazyLock::new(crate::backend::settings::settings_backups_dir_path);

/// Render the Developer Tools page as its five scroll anchors: network proxy,
/// chat logging, history & cleanup, diagnostics, and configuration.
///
/// The many logging groups cluster into user-meaningful anchors instead of one
/// tree node per group.
pub fn render_advanced_section<T: 'static>(
    developer_visible: bool,
    ui: &mut SettingsPageUi,
    window: &mut Window,
    cx: &mut Context<T>,
) -> SettingsPage {
    let logging = super::render_logging_section(ui, window, cx);
    let configuration = render_configuration_actions(SETTINGS_FILE_PATH.clone(), ui, window, cx);
    let network = super::render_proxy_section(ui, window, cx);
    let data_management = render_advanced_data_management(ui.stacked, cx);
    let mut page = SettingsPage::new(SettingsSectionId::Advanced)
        .quiet()
        .subsection(SettingsSubsectionId::ChatLogging, logging.chat_logging)
        .subsection(
            SettingsSubsectionId::HistoryCleanup,
            logging.history_cleanup,
        )
        .subsection(SettingsSubsectionId::NetworkProxy, network)
        .subsection(SettingsSubsectionId::Configuration, configuration)
        .subsection(SettingsSubsectionId::DataManagement, data_management)
        .subsection(SettingsSubsectionId::DeveloperMode, div());
    if developer_visible {
        let disclosure = ui.disclosure("advanced-diagnostics", cx);
        page = page.subsection(
            SettingsSubsectionId::Diagnostics,
            settings_group(
                "advanced-diagnostics-group",
                "Developer diagnostics",
                false,
                settings_summary_disclosure(
                    "advanced-diagnostics",
                    "Developer diagnostics",
                    "Developer-only live metrics and reports",
                    &disclosure,
                    cx,
                    |cx| super::render_debug_section_live(cx),
                ),
            ),
        );
    }
    page
}

fn render_advanced_data_management<T: 'static>(
    stacked: bool,
    cx: &mut Context<T>,
) -> impl IntoElement + use<T> {
    let clear = super::STREAMER_MODE_DATA_SETTINGS[0];
    let reset = super::STREAMER_MODE_DATA_SETTINGS[1];
    settings_group(
        "advanced-data-management",
        "Data management",
        true,
        div()
            .flex()
            .flex_col()
            .child(setting_target(
                clear,
                crate::components::settings::widgets::setting_compact_row(
                    clear.id,
                    clear.label,
                    Some("Clear cached messages while keeping your preferences."),
                    quiet_action_button(clear.control_id, "Clear", true, cx, |cx| {
                        backend::push_ui_action(backend::UiAction::ConfirmClearMessageCache);
                        cx.notify();
                    })
                    .aria_label("Clear message cache"),
                    false,
                    None,
                    stacked,
                ),
            ))
            .child(setting_target(
                reset,
                crate::components::settings::widgets::setting_compact_row(
                    reset.id,
                    reset.label,
                    Some("Restore defaults. You will be asked to confirm."),
                    quiet_action_button(reset.control_id, "Reset", true, cx, |cx| {
                        backend::push_ui_action(backend::UiAction::ConfirmResetSettings);
                        cx.notify();
                    })
                    .aria_label("Reset all settings"),
                    false,
                    None,
                    stacked,
                ),
            )),
    )
}

fn render_authenticated_account<T: 'static>(
    auth_state: &backend::AuthState,
    active_account: Option<&Account>,
    auth_verified: bool,
    anonymous_mode: bool,
    login_modal_state: Option<Entity<LoginModalState>>,
    ui: &SettingsPageUi,
    cx: &mut Context<T>,
) -> impl IntoElement + use<T> {
    let display_name = auth_state
        .display_name
        .clone()
        .or_else(|| active_account.and_then(|account| account.display_name.clone()))
        .or(auth_state.username.clone())
        .or_else(|| active_account.map(|account| account.username.clone()))
        .unwrap_or_else(|| "Unknown".to_string());

    let profile_image_url = auth_state
        .profile_image_url
        .as_deref()
        .or_else(|| active_account.and_then(|account| account.profile_image_url.as_deref()));
    let missing_runtime_token =
        active_account.is_some() && backend::bridge::active_account_missing_runtime_token();
    let needs_reauth = auth_state.needs_reauth || missing_runtime_token;
    let detail_text = if anonymous_mode {
        Some("Switch back to an account from the top bar to restore sending, Helix calls, and account-backed emotes.".to_string())
    } else if needs_reauth {
        Some(auth_state.error.clone().unwrap_or_else(|| {
            "Log in again to restore this account before using authenticated features.".to_string()
        }))
    } else {
        auth_state.error.clone()
    };
    let status_text = if anonymous_mode {
        "Anonymous chat active"
    } else if auth_verified {
        "Connected via Twitch"
    } else if auth_state.loading {
        "Connecting to Twitch…"
    } else if needs_reauth {
        "Re-authentication required"
    } else {
        "Saved Twitch account"
    };

    let help = detail_text.unwrap_or_else(|| status_text.to_string());
    setting_target(
        ACCOUNT_SIGN_IN,
        setting_row_with_control_width(
            "account-sign-in-row",
            "Twitch account",
            Some(&help),
            div()
                .min_w_0()
                .flex()
                .flex_wrap()
                .items_center()
                .justify_end()
                .gap_2()
                .child(render_profile_avatar(
                    profile_image_url,
                    &display_name,
                    28.0,
                ))
                .child(
                    div()
                        .min_w_0()
                        .max_w(px(136.0))
                        .text_sm()
                        .text_color(colors::text_primary())
                        .text_ellipsis()
                        .child(display_name),
                )
                .when(login_modal_state.is_some(), |controls| {
                    let login_modal_for_reauth = login_modal_state.clone();
                    controls
                        .when(needs_reauth, |controls| {
                            controls.child(render_button("Re-authenticate", false, cx, move |cx| {
                                match login_modal_for_reauth.clone() {
                                    Some(login_modal_state) => {
                                        open_account_login(&login_modal_state, cx);
                                    }
                                    None => backend::ui_actions::push_pending_open_login(),
                                }
                                cx.notify();
                            }))
                        })
                        .child(render_button("Log out", true, cx, |cx| {
                            backend::logout();
                            cx.notify();
                        }))
                }),
            false,
            None,
            ui.stacked,
            400.0,
        ),
    )
}

/// Read-only login prompt for standalone popout (no login modal available)
fn render_login_prompt_readonly<T: 'static>(
    ui: &SettingsPageUi,
    _cx: &mut Context<T>,
) -> impl IntoElement + use<T> {
    setting_target(
        ACCOUNT_SIGN_IN,
        setting_row_with_control_width(
            "account-sign-in-row",
            "Twitch account",
            Some("You are not logged in. Use the main window to connect your Twitch account."),
            div()
                .text_sm()
                .text_color(colors::text_secondary())
                .child("Unavailable"),
            false,
            None,
            ui.stacked,
            400.0,
        ),
    )
}

fn render_login_prompt<T: 'static>(
    login_modal_state: Entity<LoginModalState>,
    ui: &SettingsPageUi,
    cx: &mut Context<T>,
) -> impl IntoElement + use<T> {
    setting_target(
        ACCOUNT_SIGN_IN,
        setting_row_with_control_width(
            "account-sign-in-row",
            "Twitch account",
            Some("Connect your Twitch account to access account-backed features."),
            settings_button_keyed(
                "login-twitch-settings-btn",
                "Log in with Twitch",
                true,
                cx,
                move |cx| {
                    login_modal_state.update(cx, |state, _| state.open());
                    cx.notify();
                },
            ),
            false,
            None,
            ui.stacked,
            400.0,
        ),
    )
}

fn open_account_login(login_modal: &Entity<LoginModalState>, cx: &mut App) {
    login_modal.update(cx, |state, cx| {
        if !state.is_open {
            state.open();
            cx.notify();
        }
    });
}

/// Render a list of all accounts with switch/remove buttons
fn render_accounts_list<T: 'static>(
    accounts: Vec<Account>,
    login_modal_state: Entity<LoginModalState>,
    cx: &mut Context<T>,
) -> impl IntoElement + use<T> {
    let login_modal_for_action = login_modal_state.clone();
    let login_modal_for_accessible_action = login_modal_state.clone();
    div()
        .flex()
        .flex_col()
        .gap_2()
        .children(
            accounts
                .iter()
                .map(|account| render_account_item(account, login_modal_state.clone(), cx)),
        )
        .child(
            div()
                .id("add-account-settings-btn")
                .role(Role::Button)
                .map(|control| {
                    crate::components::settings::widgets::settings_keyboard_control(control, cx)
                })
                .a11y_synthetic_children(crate::components::ui::accessibility::apply_description)
                .aria_label("Add account")
                .flex()
                .items_center()
                .gap_2()
                .p_2()
                .rounded(Radii::md())
                .cursor_pointer()
                .hover(|s| s.bg(colors::hover_surface()))
                .on_activate(move |_event, _window, cx| {
                    login_modal_for_action.update(cx, |state, _| {
                        state.open();
                    });
                })
                .on_a11y_action(AccessibleAction::Click, move |_action, _window, cx| {
                    login_modal_for_accessible_action.update(cx, |state, _| {
                        state.open();
                    });
                })
                .child(
                    div()
                        .w(px(32.0))
                        .h(px(32.0))
                        .rounded_full()
                        .bg({
                            let mut c = colors::accent();
                            c.a = 0.15;
                            c
                        })
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(
                            div()
                                .text_base()
                                .font_weight(FontWeight::BOLD)
                                .text_color(colors::accent_text())
                                .child("+"),
                        ),
                )
                .child(
                    div()
                        .text_sm()
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(colors::accent_text())
                        .child("Add Account"),
                ),
        )
}

/// Render a single account item with switch/remove actions
fn render_account_item<T: 'static>(
    account: &Account,
    login_modal: Entity<LoginModalState>,
    cx: &mut Context<T>,
) -> impl IntoElement + use<T> {
    let login_modal_for_accessible = login_modal.clone();
    let username = account.username.clone();
    let display_name = account
        .display_name
        .clone()
        .unwrap_or_else(|| account.username.clone());
    let profile_url = account.profile_image_url.clone();
    let is_active = account.is_active;
    let missing_runtime_token = backend::bridge::saved_account_missing_runtime_token(&username);
    let needs_reauth =
        missing_runtime_token || (is_active && backend::bridge::active_account_needs_reauth());
    let username_for_switch = username.clone();
    let username_for_switch_accessible = username.clone();
    let username_for_remove = username.clone();
    let username_for_remove_accessible = username.clone();
    let display_name_for_remove = display_name.clone();
    let display_name_for_remove_accessible = display_name.clone();
    let needs_reauth_for_switch = needs_reauth;
    let switch_label = if needs_reauth {
        "Re-authenticate"
    } else {
        "Switch account"
    };

    div()
        .id(SharedString::from(format!("settings-account-{}", username)))
        .flex()
        .items_center()
        .gap_3()
        .p_2()
        .rounded(Radii::md())
        .bg(colors::bg_surface())
        .border_1()
        .border_color(if is_active {
            if needs_reauth {
                colors::error()
            } else {
                colors::accent()
            }
        } else {
            colors::border()
        })
        .child(render_profile_avatar(
            profile_url.as_deref(),
            &display_name,
            32.0,
        ))
        .child(
            div()
                .flex_1()
                .flex()
                .flex_col()
                .overflow_hidden()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(
                            div()
                                .text_sm()
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(colors::text_primary())
                                .text_ellipsis()
                                .child(display_name),
                        )
                        .when(is_active, |el| {
                            el.child(
                                div()
                                    .px_1p5()
                                    .py_0p5()
                                    .rounded(Radii::sm())
                                    .bg(if needs_reauth {
                                        colors::error().opacity(0.12)
                                    } else {
                                        let mut c = colors::accent();
                                        c.a = 0.15;
                                        c
                                    })
                                    .text_xs()
                                    .text_color(if needs_reauth {
                                        colors::error()
                                    } else {
                                        colors::accent()
                                    })
                                    .child(if needs_reauth {
                                        "Needs re-auth"
                                    } else {
                                        "Active"
                                    }),
                            )
                        }),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(if needs_reauth {
                            colors::error()
                        } else {
                            colors::text_muted()
                        })
                        .child(if needs_reauth {
                            "Re-authentication required".to_string()
                        } else {
                            format!("@{}", username)
                        }),
                ),
        )
        .when(!is_active, |el| {
            el.child(
                div()
                    .id(SharedString::from(format!(
                        "switch-{}",
                        username_for_switch.clone()
                    )))
                    .role(Role::Button)
                    .map(|control| {
                        crate::components::settings::widgets::settings_keyboard_control(control, cx)
                    })
                    .a11y_synthetic_children(
                        crate::components::ui::accessibility::apply_description,
                    )
                    .aria_label(switch_label)
                    .px_2()
                    .py_1()
                    .rounded(Radii::sm())
                    .bg(if needs_reauth {
                        colors::error().opacity(0.12)
                    } else {
                        colors::bg_elevated()
                    })
                    .cursor_pointer()
                    .hover(|s| s.bg(colors::bg_hover()))
                    .on_activate(move |_event, window, cx| {
                        if needs_reauth_for_switch {
                            open_account_login(&login_modal, cx);
                            window.refresh();
                        } else {
                            backend::switch_account(&username_for_switch);
                        }
                    })
                    .on_a11y_action(AccessibleAction::Click, move |_action, window, cx| {
                        if needs_reauth_for_switch {
                            open_account_login(&login_modal_for_accessible, cx);
                            window.refresh();
                        } else {
                            backend::switch_account(&username_for_switch_accessible);
                        }
                    })
                    .child(
                        div()
                            .text_xs()
                            .text_color(if needs_reauth {
                                colors::error()
                            } else {
                                colors::text_secondary()
                            })
                            .child(if needs_reauth {
                                "Re-authenticate"
                            } else {
                                "Switch"
                            }),
                    ),
            )
        })
        .child(
            div()
                .id(SharedString::from(format!(
                    "remove-{}",
                    username_for_remove.clone()
                )))
                .role(Role::Button)
                .map(|control| {
                    crate::components::settings::widgets::settings_keyboard_control(control, cx)
                })
                .a11y_synthetic_children(crate::components::ui::accessibility::apply_description)
                .aria_label("Remove account")
                .px_2()
                .py_1()
                .rounded(Radii::sm())
                .cursor_pointer()
                .hover(|s| {
                    let mut c = colors::error();
                    c.a = 0.1;
                    s.bg(c)
                })
                .on_activate(move |_event, _window, _cx| {
                    backend::push_ui_action(backend::UiAction::ConfirmRemoveAccount {
                        username: username_for_remove.clone(),
                        display_name: display_name_for_remove.clone(),
                    });
                })
                .on_a11y_action(AccessibleAction::Click, move |_action, _window, _cx| {
                    backend::push_ui_action(backend::UiAction::ConfirmRemoveAccount {
                        username: username_for_remove_accessible.clone(),
                        display_name: display_name_for_remove_accessible.clone(),
                    });
                })
                .child(div().text_xs().text_color(colors::error()).child("Remove")),
        )
}

pub fn render_settings_window_section<T: 'static>(
    ui: &SettingsPageUi,
    cx: &mut Context<T>,
) -> impl IntoElement + use<T> {
    use crate::backend::lock_utils::MutexExt;
    use crate::backend::settings::SETTINGS;

    let (is_on, minimize_to_tray, close_to_tray) = {
        let settings = SETTINGS.lock_or_recover();
        (
            settings.open_settings_as_popout,
            settings.minimize_to_tray,
            settings.close_to_tray,
        )
    };
    let defaults = settings_defaults();
    let default = defaults.open_settings_as_popout;
    settings_group(
        "general-settings-window",
        "Window & startup",
        false,
        div()
            .flex()
            .flex_col()
            .child(setting_target(
                SETTINGS_AS_POPOUT,
                crate::components::settings::widgets::setting_compact_row(
                    "settings-window-row",
                    SETTINGS_AS_POPOUT.label,
                    Some("Open Settings in a separate window instead of an overlay."),
                    labelled_toggle(
                        SETTINGS_AS_POPOUT.control_id,
                        SETTINGS_AS_POPOUT.label,
                        is_on,
                        cx,
                        |new_state, cx| {
                            use crate::backend::lock_utils::MutexExt;
                            use crate::backend::settings::SETTINGS;
                            SETTINGS.lock_or_recover().open_settings_as_popout = new_state;
                            crate::backend::settings::save_settings();
                            cx.notify();
                        },
                    ),
                    is_on != default,
                    (is_on != default).then(|| {
                        setting_reset(
                            "reset-settings-window",
                            SETTINGS_AS_POPOUT.label,
                            cx,
                            move |cx| {
                                use crate::backend::lock_utils::MutexExt;
                                use crate::backend::settings::SETTINGS;
                                SETTINGS.lock_or_recover().open_settings_as_popout = default;
                                crate::backend::settings::save_settings();
                                cx.notify();
                            },
                        )
                    }),
                    ui.stacked,
                ),
            ))
            .when(cfg!(target_os = "windows"), |content| {
                content
                    .child(setting_target(
                        MINIMIZE_TO_TRAY,
                        crate::components::settings::widgets::setting_compact_row(
                            "minimize-to-tray-row",
                            MINIMIZE_TO_TRAY.label,
                            Some("Hide Echo in the notification area when it is minimized."),
                            labelled_toggle(
                                MINIMIZE_TO_TRAY.control_id,
                                MINIMIZE_TO_TRAY.label,
                                minimize_to_tray,
                                cx,
                                |enabled, cx| {
                                    crate::backend::settings::set_minimize_to_tray(enabled);
                                    cx.notify();
                                },
                            ),
                            minimize_to_tray != defaults.minimize_to_tray,
                            (minimize_to_tray != defaults.minimize_to_tray).then(|| {
                                let value = defaults.minimize_to_tray;
                                setting_reset(
                                    "reset-minimize-to-tray",
                                    MINIMIZE_TO_TRAY.label,
                                    cx,
                                    move |cx| {
                                        crate::backend::settings::set_minimize_to_tray(value);
                                        cx.notify();
                                    },
                                )
                            }),
                            ui.stacked,
                        ),
                    ))
                    .child(setting_target(
                        CLOSE_TO_TRAY,
                        crate::components::settings::widgets::setting_compact_row(
                            "close-to-tray-row",
                            CLOSE_TO_TRAY.label,
                            Some("Hide Echo in the notification area when its window is closed."),
                            labelled_toggle(
                                CLOSE_TO_TRAY.control_id,
                                CLOSE_TO_TRAY.label,
                                close_to_tray,
                                cx,
                                |enabled, cx| {
                                    crate::backend::settings::set_close_to_tray(enabled);
                                    cx.notify();
                                },
                            ),
                            close_to_tray != defaults.close_to_tray,
                            (close_to_tray != defaults.close_to_tray).then(|| {
                                let value = defaults.close_to_tray;
                                setting_reset(
                                    "reset-close-to-tray",
                                    CLOSE_TO_TRAY.label,
                                    cx,
                                    move |cx| {
                                        crate::backend::settings::set_close_to_tray(value);
                                        cx.notify();
                                    },
                                )
                            }),
                            ui.stacked,
                        ),
                    ))
            }),
    )
}

fn render_window_settings<T: 'static>(
    always_on_top: bool,
    window_transparent: bool,
    stacked: bool,
    cx: &mut Context<T>,
) -> impl IntoElement + use<T> {
    let defaults = settings_defaults();
    div()
        .flex()
        .flex_col()
        .child(setting_target(
            WINDOW_TRANSPARENT,
            crate::components::settings::widgets::setting_compact_row(
                "window-transparent-row",
                WINDOW_TRANSPARENT.label,
                Some("Apply compositor transparency to the main Echo window."),
                labelled_toggle(
                    WINDOW_TRANSPARENT.control_id,
                    WINDOW_TRANSPARENT.label,
                    window_transparent,
                    cx,
                    |enabled, cx| {
                        crate::backend::settings::set_window_transparent(enabled);
                        cx.notify();
                    },
                ),
                window_transparent != defaults.window_transparent,
                (window_transparent != defaults.window_transparent).then(|| {
                    let value = defaults.window_transparent;
                    setting_reset(
                        "reset-window-transparent",
                        WINDOW_TRANSPARENT.label,
                        cx,
                        move |cx| {
                            crate::backend::settings::set_window_transparent(value);
                            cx.notify();
                        },
                    )
                }),
                stacked,
            ),
        ))
        .child(setting_target(
            ALWAYS_ON_TOP,
            crate::components::settings::widgets::setting_compact_row(
                "always-on-top-row",
                ALWAYS_ON_TOP.label,
                Some("Keep Echo above other windows."),
                labelled_toggle(
                    ALWAYS_ON_TOP.control_id,
                    ALWAYS_ON_TOP.label,
                    always_on_top,
                    cx,
                    |enabled, cx| {
                        crate::backend::settings::set_always_on_top(enabled);
                        crate::components::ui::refresh_windows_for_always_on_top(cx);
                        cx.notify();
                    },
                ),
                always_on_top != defaults.always_on_top,
                (always_on_top != defaults.always_on_top).then(|| {
                    let value = defaults.always_on_top;
                    setting_reset("reset-always-on-top", ALWAYS_ON_TOP.label, cx, move |cx| {
                        crate::backend::settings::set_always_on_top(value);
                        crate::components::ui::refresh_windows_for_always_on_top(cx);
                        cx.notify();
                    })
                }),
                stacked,
            ),
        ))
}

#[allow(clippy::too_many_arguments)]
fn render_overlay_settings<T: 'static>(
    click_through: bool,
    ghost_mode: bool,
    max_messages: usize,
    idle_fade_secs: u64,
    min_opacity: f32,
    stacked: bool,
    max_messages_select: &crate::components::settings::widgets::SettingsSelectState,
    idle_fade_select: &crate::components::settings::widgets::SettingsSelectState,
    min_opacity_select: &crate::components::settings::widgets::SettingsSelectState,
    cx: &mut Context<T>,
) -> impl IntoElement + use<T> {
    let max_messages_value = max_messages.to_string();
    let fade_value = idle_fade_secs.to_string();
    let min_opacity_value = ((min_opacity * 100.0).round() as u8).to_string();

    let defaults = settings_defaults();
    div()
        .flex()
        .flex_col()
        .child(setting_target(
            OVERLAY_CLICK_THROUGH,
            crate::components::settings::widgets::setting_compact_row(
                "overlay-click-through-row",
                OVERLAY_CLICK_THROUGH.label,
                Some("Ignore mouse input while the overlay stays visible."),
                labelled_toggle(
                    OVERLAY_CLICK_THROUGH.control_id,
                    OVERLAY_CLICK_THROUGH.label,
                    click_through,
                    cx,
                    |enabled, cx| {
                        crate::backend::settings::set_overlay_click_through(enabled);
                        cx.notify();
                    },
                ),
                click_through != defaults.overlay_click_through,
                (click_through != defaults.overlay_click_through).then(|| {
                    let value = defaults.overlay_click_through;
                    setting_reset(
                        "reset-overlay-click-through",
                        OVERLAY_CLICK_THROUGH.label,
                        cx,
                        move |cx| {
                            crate::backend::settings::set_overlay_click_through(value);
                            cx.notify();
                        },
                    )
                }),
                stacked,
            ),
        ))
        .child(setting_target(
            OVERLAY_GHOST_MODE,
            crate::components::settings::widgets::setting_compact_row(
                "overlay-ghost-mode-row",
                OVERLAY_GHOST_MODE.label,
                Some("Hide the overlay header and use a lighter backdrop."),
                labelled_toggle(
                    OVERLAY_GHOST_MODE.control_id,
                    OVERLAY_GHOST_MODE.label,
                    ghost_mode,
                    cx,
                    |enabled, cx| {
                        crate::backend::settings::set_overlay_ghost_mode(enabled);
                        cx.notify();
                    },
                ),
                ghost_mode != defaults.overlay_ghost_mode,
                (ghost_mode != defaults.overlay_ghost_mode).then(|| {
                    let value = defaults.overlay_ghost_mode;
                    setting_reset(
                        "reset-overlay-ghost-mode",
                        OVERLAY_GHOST_MODE.label,
                        cx,
                        move |cx| {
                            crate::backend::settings::set_overlay_ghost_mode(value);
                            cx.notify();
                        },
                    )
                }),
                stacked,
            ),
        ))
        .child(setting_target(
            OVERLAY_MAX_MESSAGES,
            setting_row(
                "overlay-max-messages-row",
                OVERLAY_MAX_MESSAGES.label,
                Some("Choose how many recent messages the overlay keeps on screen."),
                settings_select(
                    OVERLAY_MAX_MESSAGES.control_id,
                    OVERLAY_MAX_MESSAGES.label,
                    &[
                        SelectOption::new("10", "10"),
                        SelectOption::new("15", "15"),
                        SelectOption::new("20", "20"),
                        SelectOption::new("30", "30"),
                        SelectOption::new("50", "50"),
                    ],
                    &max_messages_value,
                    max_messages_select,
                    cx,
                    |value, cx| {
                        if let Ok(count) = value.parse::<usize>() {
                            crate::backend::settings::set_overlay_max_messages(count);
                            cx.notify();
                        }
                    },
                ),
                max_messages != defaults.overlay_max_messages as usize,
                (max_messages != defaults.overlay_max_messages as usize).then(|| {
                    let value = defaults.overlay_max_messages as usize;
                    setting_reset(
                        "reset-overlay-max-messages",
                        OVERLAY_MAX_MESSAGES.label,
                        cx,
                        move |cx| {
                            crate::backend::settings::set_overlay_max_messages(value);
                            cx.notify();
                        },
                    )
                }),
                stacked,
            ),
        ))
        .child(setting_target(
            OVERLAY_IDLE_FADE,
            setting_row(
                "overlay-idle-fade-row",
                OVERLAY_IDLE_FADE.label,
                Some("Delay before the overlay starts fading when chat is quiet."),
                settings_select(
                    OVERLAY_IDLE_FADE.control_id,
                    OVERLAY_IDLE_FADE.label,
                    &[
                        SelectOption::new("0", "Never"),
                        SelectOption::new("15", "15s"),
                        SelectOption::new("30", "30s"),
                        SelectOption::new("60", "60s"),
                    ],
                    &fade_value,
                    idle_fade_select,
                    cx,
                    |value, cx| {
                        if let Ok(secs) = value.parse::<u64>() {
                            crate::backend::settings::set_overlay_idle_fade_secs(secs);
                            cx.notify();
                        }
                    },
                ),
                idle_fade_secs != defaults.overlay_idle_fade_secs as u64,
                (idle_fade_secs != defaults.overlay_idle_fade_secs as u64).then(|| {
                    let value = defaults.overlay_idle_fade_secs as u64;
                    setting_reset(
                        "reset-overlay-idle-fade",
                        OVERLAY_IDLE_FADE.label,
                        cx,
                        move |cx| {
                            crate::backend::settings::set_overlay_idle_fade_secs(value);
                            cx.notify();
                        },
                    )
                }),
                stacked,
            ),
        ))
        .child(setting_target(
            OVERLAY_MIN_OPACITY,
            setting_row(
                "overlay-min-opacity-row",
                OVERLAY_MIN_OPACITY.label,
                Some("How visible the overlay remains after the idle fade completes."),
                settings_select(
                    OVERLAY_MIN_OPACITY.control_id,
                    OVERLAY_MIN_OPACITY.label,
                    &[
                        SelectOption::new("10", "10%"),
                        SelectOption::new("30", "30%"),
                        SelectOption::new("50", "50%"),
                    ],
                    &min_opacity_value,
                    min_opacity_select,
                    cx,
                    |value, cx| {
                        if let Ok(percent) = value.parse::<u8>() {
                            crate::backend::settings::set_overlay_min_opacity(
                                percent as f32 / 100.0,
                            );
                            cx.notify();
                        }
                    },
                ),
                min_opacity_value.parse::<u8>().unwrap_or_default()
                    != defaults.overlay_min_opacity_percent,
                (min_opacity_value.parse::<u8>().unwrap_or_default()
                    != defaults.overlay_min_opacity_percent)
                    .then(|| {
                        let value = defaults.overlay_min_opacity_percent as f32 / 100.0;
                        setting_reset(
                            "reset-overlay-min-opacity",
                            OVERLAY_MIN_OPACITY.label,
                            cx,
                            move |cx| {
                                crate::backend::settings::set_overlay_min_opacity(value);
                                cx.notify();
                            },
                        )
                    }),
                stacked,
            ),
        ))
        .child(setting_target(
            OVERLAY_SAVED_POSITION,
            setting_row(
                "overlay-saved-position-row",
                OVERLAY_SAVED_POSITION.label,
                Some("Reset the saved overlay position for the next open."),
                render_button("Reset position", false, cx, |cx| {
                    crate::backend::settings::clear_overlay_window_position();
                    cx.notify();
                }),
                false,
                None,
                stacked,
            ),
        ))
}

fn general_field_index(field: GeneralSettingsField) -> usize {
    match field {
        GeneralSettingsField::BrowserPath => 0,
        GeneralSettingsField::CustomPlayerPath => 1,
        GeneralSettingsField::StreamlinkPath => 2,
        GeneralSettingsField::StreamlinkCustomQuality => 3,
        GeneralSettingsField::StreamlinkExtraArgs => 4,
    }
}

fn general_field_from_index(index: usize) -> GeneralSettingsField {
    match index {
        0 => GeneralSettingsField::BrowserPath,
        1 => GeneralSettingsField::CustomPlayerPath,
        2 => GeneralSettingsField::StreamlinkPath,
        3 => GeneralSettingsField::StreamlinkCustomQuality,
        _ => GeneralSettingsField::StreamlinkExtraArgs,
    }
}

fn general_field_value(editor: &GeneralSettingsEditor, field: GeneralSettingsField) -> &str {
    match field {
        GeneralSettingsField::BrowserPath => &editor.browser_path,
        GeneralSettingsField::CustomPlayerPath => &editor.custom_player_path,
        GeneralSettingsField::StreamlinkPath => &editor.streamlink_path,
        GeneralSettingsField::StreamlinkCustomQuality => &editor.streamlink_custom_quality,
        GeneralSettingsField::StreamlinkExtraArgs => &editor.streamlink_extra_args,
    }
}

fn general_field_authority(field: GeneralSettingsField) -> String {
    match field {
        GeneralSettingsField::BrowserPath => {
            crate::backend::settings::get_browser_path().unwrap_or_default()
        }
        GeneralSettingsField::CustomPlayerPath => {
            crate::backend::settings::get_custom_player_path().unwrap_or_default()
        }
        GeneralSettingsField::StreamlinkPath => {
            crate::backend::settings::get_streamlink_path().unwrap_or_default()
        }
        GeneralSettingsField::StreamlinkCustomQuality => {
            crate::backend::settings::get_streamlink_custom_quality()
        }
        GeneralSettingsField::StreamlinkExtraArgs => {
            crate::backend::settings::get_streamlink_extra_args()
        }
    }
}

pub(crate) fn release_general_settings_inputs_for_host(owner: EntityId) {
    let removed = GENERAL_SETTINGS_INPUTS
        .try_with(|inputs| inputs.borrow_mut().remove(&owner))
        .ok()
        .flatten();
    drop(removed);
}

fn general_field_input<T: 'static>(
    field: GeneralSettingsField,
    cx: &Context<T>,
) -> Option<Entity<InputState>> {
    let owner = cx.entity_id();
    GENERAL_SETTINGS_INPUTS.with(|inputs| {
        inputs
            .borrow()
            .get(&owner)
            .and_then(|inputs| inputs.fields[general_field_index(field)].clone())
    })
}

fn sync_general_field_input<T: 'static>(
    field: GeneralSettingsField,
    editor: &GeneralSettingsEditor,
    window: &mut Window,
    cx: &mut Context<T>,
) {
    let Some(input) = general_field_input(field, cx) else {
        return;
    };
    let text = crate::components::ui::input::normalize_single_line_text(general_field_value(
        editor, field,
    ));
    if input.read(cx).value() != text {
        input.update(cx, |input, cx| input.set_value(text, window, cx));
    }
}

fn ensure_general_settings_inputs<T: 'static>(
    editor: &GeneralSettingsEditor,
    visible_fields: &[GeneralSettingsField],
    window: &mut Window,
    cx: &mut Context<T>,
) {
    let owner = cx.entity_id();
    let wanted: [bool; 5] = std::array::from_fn(|index| {
        visible_fields
            .iter()
            .any(|field| general_field_index(*field) == index)
    });
    let mut cached = GENERAL_SETTINGS_INPUTS
        .with(|inputs| inputs.borrow_mut().remove(&owner))
        .unwrap_or_else(|| GeneralSettingsInputs {
            fields: std::array::from_fn(|_| None),
            subscriptions: std::array::from_fn(|_| None),
        });
    let mut make_input = |field: GeneralSettingsField,
                          placeholder: &'static str,
                          value: String,
                          cx: &mut Context<T>| {
        let input = cx.new(|cx| {
            InputState::new(window, cx)
                .with_placeholder(placeholder)
                .with_value(value)
        });
        let subscription = cx.subscribe_in(
            &input,
            window,
            move |_this, input, event: &InputEvent, window, cx| {
                let authority = general_field_authority(field);
                match event {
                    InputEvent::Focus => {
                        update_general_settings_editor(|editor| editor.focused = Some(field));
                    }
                    InputEvent::Change => {
                        let raw = input.read(cx).value().to_string();
                        let value = match normalize_general_editor_text(field, &raw) {
                            Ok(value) => value,
                            Err(message) => {
                                let snapshot = general_settings_editor_snapshot();
                                input.update(cx, |input, cx| {
                                    input.set_value(
                                        general_field_value(&snapshot.draft, field).to_string(),
                                        window,
                                        cx,
                                    );
                                });
                                update_general_settings_editor(|editor| {
                                    let (_, status, status_is_error) =
                                        general_editor_field_parts(editor, field);
                                    *status = Some(message.to_string());
                                    *status_is_error = true;
                                });
                                cx.notify();
                                return;
                            }
                        };
                        if input.read(cx).value() != value {
                            input
                                .update(cx, |input, cx| input.set_value(value.clone(), window, cx));
                        }
                        let snapshot = general_settings_editor_snapshot();
                        if crate::components::ui::input::normalize_single_line_text(
                            general_field_value(&snapshot.draft, field),
                        ) == value
                        {
                            return;
                        }
                        update_general_settings_editor(|editor| {
                            editor.dirty[general_field_index(field)] = value != authority;
                            let (target, status, status_is_error) =
                                general_editor_field_parts(editor, field);
                            *target = value.clone();
                            *status = None;
                            *status_is_error = false;
                        });
                    }
                    InputEvent::PressEnter => {
                        let snapshot = general_settings_editor_snapshot();
                        if snapshot.draft.dirty[general_field_index(field)] {
                            save_general_settings_field(snapshot.generation, field);
                        }
                    }
                    InputEvent::Blur => {
                        let snapshot = general_settings_editor_snapshot();
                        if snapshot.draft.dirty[general_field_index(field)] {
                            save_general_settings_field(snapshot.generation, field);
                        }
                        update_general_settings_editor(|editor| {
                            if editor.focused == Some(field) {
                                editor.focused = None;
                            }
                        });
                    }
                }
                cx.notify();
            },
        );
        (input, subscription)
    };
    for (index, is_wanted) in wanted.into_iter().enumerate() {
        if !is_wanted {
            cached.fields[index] = None;
            cached.subscriptions[index] = None;
            continue;
        }
        if cached.fields[index].is_none() {
            let field = general_field_from_index(index);
            let (placeholder, value) = match field {
                GeneralSettingsField::BrowserPath => {
                    ("Browser executable", editor.browser_path.clone())
                }
                GeneralSettingsField::CustomPlayerPath => {
                    ("Player executable", editor.custom_player_path.clone())
                }
                GeneralSettingsField::StreamlinkPath => {
                    ("streamlink from PATH", editor.streamlink_path.clone())
                }
                GeneralSettingsField::StreamlinkCustomQuality => {
                    ("Custom quality", editor.streamlink_custom_quality.clone())
                }
                GeneralSettingsField::StreamlinkExtraArgs => {
                    ("No extra arguments", editor.streamlink_extra_args.clone())
                }
            };
            let (input, subscription) = make_input(field, placeholder, value, cx);
            cached.fields[index] = Some(input);
            cached.subscriptions[index] = Some(subscription);
        }
    }
    GENERAL_SETTINGS_INPUTS.with(|entries| {
        entries.borrow_mut().insert(owner, cached);
    });
    for field in visible_fields {
        sync_general_field_input(*field, editor, window, cx);
    }
}

fn render_general_input<T: 'static>(
    field: GeneralSettingsField,
    label: &'static str,
    _window: &mut Window,
    cx: &mut Context<T>,
) -> impl IntoElement + use<T> {
    div()
        .w_full()
        .min_w_0()
        .h(px(SettingsMetrics::INPUT_MIN_HEIGHT))
        .font_family("monospace")
        .when_some(general_field_input(field, cx), |element, input| {
            element
                .capture_action(cx.listener(move |_this, _: &Escape, window, cx| {
                    revert_general_editor_field(field);
                    sync_general_field_input(
                        field,
                        &general_settings_editor_snapshot().draft,
                        window,
                        cx,
                    );
                    cx.stop_propagation();
                    cx.notify();
                }))
                .child(Input::new(&input).accessibility_label(label))
        })
}

fn general_editor_summary(
    editor: &EditorSnapshot<GeneralSettingsEditor>,
    fields: &[GeneralSettingsField],
    plain: &str,
) -> String {
    if editor.needs_review {
        format!("Needs review · {plain}")
    } else if fields
        .iter()
        .any(|field| editor.draft.dirty[general_field_index(*field)])
    {
        format!("Unsaved draft · {plain}")
    } else {
        plain.to_string()
    }
}

fn render_links_surface<T: 'static>(
    editor: EditorSnapshot<GeneralSettingsEditor>,
    more: &crate::components::settings::widgets::SettingsDisclosureState,
    mode_select: &SettingsSelectState,
    stacked: bool,
    window: &mut Window,
    cx: &mut Context<T>,
) -> impl IntoElement + use<T> {
    let open_links_incognito = crate::backend::settings::get_open_links_incognito();
    let ask_before_opening_links = !crate::backend::settings::get_skip_open_link_confirmation();
    let trusted_link_domain_count = crate::backend::settings::get_trusted_link_domains().len();
    let defaults = settings_defaults();
    let custom_browser = editor.draft.browser_path_mode || !editor.draft.browser_path.is_empty();
    let browser_path_modified =
        crate::backend::settings::get_browser_path() != defaults.browser_path;
    let more_summary = general_editor_summary(
        &editor,
        &[GeneralSettingsField::BrowserPath],
        "Private windows, browser, and trusted domains",
    );
    settings_group(
        "links",
        "Links",
        true,
        settings_group_contents(
            "links-content",
            "Links",
            false,
            div()
                .w_full()
                .min_w_0()
                .flex()
                .flex_col()
                .child(setting_target(
                    ASK_BEFORE_OPENING_LINKS,
                    crate::components::settings::widgets::setting_compact_row(
                        ASK_BEFORE_OPENING_LINKS.id,
                        ASK_BEFORE_OPENING_LINKS.label,
                        Some("Ask before opening a chat link or trusting its domain."),
                        labelled_toggle(
                            ASK_BEFORE_OPENING_LINKS.control_id,
                            ASK_BEFORE_OPENING_LINKS.label,
                            ask_before_opening_links,
                            cx,
                            |enabled, cx| {
                                crate::backend::settings::set_skip_open_link_confirmation(!enabled);
                                cx.notify();
                            },
                        ),
                        ask_before_opening_links != !defaults.skip_open_link_confirmation,
                        (ask_before_opening_links != !defaults.skip_open_link_confirmation).then(|| {
                            let value = !defaults.skip_open_link_confirmation;
                            setting_reset("reset-ask-before-opening-links", ASK_BEFORE_OPENING_LINKS.label, cx, move |cx| {
                                crate::backend::settings::set_skip_open_link_confirmation(!value);
                                cx.notify();
                            })
                        }),
                        stacked,
                    ),
                ))
                .child(settings_disclosure(
                    "links-more",
                    "Link handling",
                    Some(&more_summary),
                    usize::from(open_links_incognito != defaults.open_links_incognito)
                        + usize::from(browser_path_modified),
                    more,
                    cx,
                    move |cx| {
                        div()
                            .w_full()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap_3()
                            .child(setting_target(
                                OPEN_LINKS_INCOGNITO,
                                crate::components::settings::widgets::setting_compact_row(
                                    OPEN_LINKS_INCOGNITO.id,
                                    OPEN_LINKS_INCOGNITO.label,
                                    Some("Use a private browser window when opening chat links."),
                                    labelled_toggle(
                                        OPEN_LINKS_INCOGNITO.control_id,
                                        OPEN_LINKS_INCOGNITO.label,
                                        open_links_incognito,
                                        cx,
                                        |enabled, cx| {
                                            crate::backend::settings::set_open_links_incognito(enabled);
                                            cx.notify();
                                        },
                                    ),
                                    open_links_incognito != defaults.open_links_incognito,
                                    (open_links_incognito != defaults.open_links_incognito).then(|| {
                                        let value = defaults.open_links_incognito;
                                        setting_reset("reset-open-links-incognito", OPEN_LINKS_INCOGNITO.label, cx, move |cx| {
                                            crate::backend::settings::set_open_links_incognito(value);
                                            cx.notify();
                                        })
                                    }),
                                    stacked,
                                ),
                            ))
                            .child(
                                setting_row_with_control_width(
                                    "links-browser-mode-row",
                                    "Private-window browser",
                                    Some("Used only for private-window link launches; normal links use the OS default."),
                                    settings_select(
                                        "links-browser-mode",
                                        "Private-window browser",
                                        &[
                                            SelectOption::new("system", "Automatic"),
                                            SelectOption::new("custom", "Use a different browser"),
                                        ],
                                        if custom_browser { "custom" } else { "system" },
                                        mode_select,
                                        cx,
                                        |value, cx| {
                                            update_general_settings_editor(|editor| {
                                                editor.browser_path_mode = value == "custom";
                                                if value == "system" {
                                                    editor.browser_path.clear();
                                                    editor.dirty[general_field_index(GeneralSettingsField::BrowserPath)] = false;
                                                }
                                            });
                                            if value == "system" {
                                                crate::backend::settings::set_browser_path(None);
                                            }
                                            cx.notify();
                                        },
                                    ),
                                    browser_path_modified,
                                    browser_path_modified.then(|| setting_reset("reset-browser-path", BROWSER_PATH.label, cx, |cx| {
                                        crate::backend::settings::set_browser_path(None);
                                        apply_general_authoritative_update(|editor| {
                                            editor.browser_path.clear();
                                            editor.browser_path_mode = false;
                                            editor.dirty[general_field_index(GeneralSettingsField::BrowserPath)] = false;
                                        });
                                        cx.notify();
                                    })),
                                    stacked,
                                    260.0,
                                ),
                            )
                            .when(custom_browser || more.is_search_revealed(), |body| {
                                body.when(!custom_browser, |body| {
                                    body.child(
                                        div()
                                            .text_xs()
                                            .text_color(colors::text_muted())
                                            .child("Applies when using a different browser."),
                                    )
                                })
                                .child(render_browser_editor(editor.clone(), stacked, window, cx))
                            })
                            .when(trusted_link_domain_count > 0, |body| {
                                body.child(
                                    setting_row(
                                        "clear-trusted-domains",
                                        "Trusted domains",
                                        Some(&format!(
                                            "{trusted_link_domain_count} trusted {} will be cleared.",
                                            if trusted_link_domain_count == 1 { "domain" } else { "domains" }
                                        )),
                                        render_button("Clear trusted domains", false, cx, |cx| {
                                            crate::backend::settings::clear_trusted_link_domains();
                                            cx.notify();
                                        }),
                                        false,
                                        None,
                                        stacked,
                                    ),
                                )
                            })
                    },
                )),
        ),
    )
}

fn render_browser_editor<T: 'static>(
    editor: EditorSnapshot<GeneralSettingsEditor>,
    _stacked: bool,
    window: &mut Window,
    cx: &mut Context<T>,
) -> impl IntoElement + use<T> {
    setting_target(
        BROWSER_PATH,
        settings_group_contents(
            "links-custom-browser",
            BROWSER_PATH.label,
            false,
            div()
                .w_full()
                .min_w_0()
                .flex()
                .flex_col()
                .gap_2()
                .child(
                    div()
                        .text_size(px(SettingsMetrics::CALM_LABEL_TEXT))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(colors::text_primary())
                        .child(BROWSER_PATH.label),
                )
                .child(render_general_input(
                    GeneralSettingsField::BrowserPath,
                    BROWSER_PATH.label,
                    window,
                    cx,
                ))
                .child(
                    div()
                        .flex()
                        .flex_wrap()
                        .gap_2()
                        .child(settings_button_keyed(
                            "links-browser-save",
                            "Save",
                            false,
                            cx,
                            move |cx| {
                                let generation = general_settings_editor_snapshot().generation;
                                apply_browser_path_save(generation);
                                cx.notify();
                            },
                        ))
                        .child(settings_button_keyed(
                            "links-browser-paste",
                            "Paste",
                            false,
                            cx,
                            |cx| {
                                if let Some(text) = crate::utils::read_text(cx) {
                                    paste_general_editor_field(
                                        GeneralSettingsField::BrowserPath,
                                        text,
                                    );
                                }
                                cx.notify();
                            },
                        ))
                        .child(settings_button_keyed(
                            "links-browser-clear",
                            "Clear",
                            false,
                            cx,
                            |cx| {
                                clear_general_editor_field(GeneralSettingsField::BrowserPath);
                                cx.notify();
                            },
                        )),
                )
                .when_some(editor.draft.browser_status.clone(), |body, status| {
                    body.child(
                        div()
                            .text_xs()
                            .text_color(if editor.draft.browser_status_is_error {
                                colors::signal_red()
                            } else {
                                colors::text_muted()
                            })
                            .child(status),
                    )
                }),
        ),
    )
}

fn render_players_surface<T: 'static>(
    editor: EditorSnapshot<GeneralSettingsEditor>,
    state: &crate::components::settings::widgets::SettingsDisclosureState,
    quality_select: &SettingsSelectState,
    stacked: bool,
    window: &mut Window,
    cx: &mut Context<T>,
) -> impl IntoElement + use<T> {
    let use_custom_player = crate::backend::settings::get_use_custom_player();
    let streamlink_quality = crate::backend::settings::get_streamlink_quality();
    let summary = general_editor_summary(
        &editor,
        &[
            GeneralSettingsField::CustomPlayerPath,
            GeneralSettingsField::StreamlinkPath,
            GeneralSettingsField::StreamlinkCustomQuality,
            GeneralSettingsField::StreamlinkExtraArgs,
        ],
        "Player and Streamlink paths",
    );
    let defaults = settings_defaults();
    settings_group(
        "players",
        "Players",
        false,
        settings_group_contents(
            "players-content",
            "Players",
            false,
            settings_summary_disclosure(
                "players-editor",
                "External players",
                &summary,
                state,
                cx,
                move |cx| {
                    div()
                        .w_full()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .gap_3()
                        .when(editor.needs_review, |body| {
                            body.child(render_general_editor_recovery_status())
                        })
                        .child(setting_target(
                            USE_CUSTOM_PLAYER,
                            crate::components::settings::widgets::setting_compact_row(
                                USE_CUSTOM_PLAYER.id,
                                USE_CUSTOM_PLAYER.label,
                                Some("Prefer the configured executable for stream launches."),
                                labelled_toggle(
                                    USE_CUSTOM_PLAYER.control_id,
                                    USE_CUSTOM_PLAYER.label,
                                    use_custom_player,
                                    cx,
                                    |enabled, cx| {
                                        crate::backend::settings::set_use_custom_player(enabled);
                                        cx.notify();
                                    },
                                ),
                                use_custom_player != defaults.use_custom_player,
                                (use_custom_player != defaults.use_custom_player).then(|| {
                                    let value = defaults.use_custom_player;
                                    setting_reset(
                                        "reset-use-custom-player",
                                        USE_CUSTOM_PLAYER.label,
                                        cx,
                                        move |cx| {
                                            crate::backend::settings::set_use_custom_player(value);
                                            cx.notify();
                                        },
                                    )
                                }),
                                stacked,
                            ),
                        ))
                        .child(render_player_text_editor(
                            CUSTOM_PLAYER_PATH,
                            GeneralSettingsField::CustomPlayerPath,
                            editor.clone(),
                            "Optional command or executable path.",
                            "Save player",
                            "Paste",
                            "Clear",
                            stacked,
                            window,
                            cx,
                        ))
                        .child(render_player_text_editor(
                            STREAMLINK_PATH,
                            GeneralSettingsField::StreamlinkPath,
                            editor.clone(),
                            "Optional command or executable used by /streamlink.",
                            "Save Streamlink",
                            "Paste",
                            "Clear",
                            stacked,
                            window,
                            cx,
                        ))
                        .child(render_streamlink_quality_row(
                            streamlink_quality,
                            quality_select,
                            defaults.streamlink_quality,
                            stacked,
                            cx,
                        ))
                        .when(
                            streamlink_quality == StreamlinkQuality::Custom
                                || state.is_search_revealed(),
                            |body| {
                                body.when(streamlink_quality != StreamlinkQuality::Custom, |body| {
                                    body.child(
                                        div()
                                            .text_xs()
                                            .text_color(colors::text_muted())
                                            .child("Applies when Streamlink quality is Custom."),
                                    )
                                })
                                .child(render_player_text_editor(
                                    STREAMLINK_CUSTOM_QUALITY,
                                    GeneralSettingsField::StreamlinkCustomQuality,
                                    editor.clone(),
                                    "Quality argument used when Custom is selected.",
                                    "Save quality",
                                    "",
                                    "Clear",
                                    stacked,
                                    window,
                                    cx,
                                ))
                            },
                        )
                        .child(render_player_text_editor(
                            STREAMLINK_EXTRA_ARGS,
                            GeneralSettingsField::StreamlinkExtraArgs,
                            editor,
                            "Quoted arguments appended after the quality.",
                            "Save arguments",
                            "Paste",
                            "Clear",
                            stacked,
                            window,
                            cx,
                        ))
                },
            ),
        ),
    )
}

#[allow(clippy::too_many_arguments)]
fn render_player_text_editor<T: 'static>(
    descriptor: SettingDescriptor,
    field: GeneralSettingsField,
    editor: EditorSnapshot<GeneralSettingsEditor>,
    help: &'static str,
    save_label: &'static str,
    paste_label: &'static str,
    clear_label: &'static str,
    _stacked: bool,
    window: &mut Window,
    cx: &mut Context<T>,
) -> impl IntoElement + use<T> {
    let (status, status_is_error) = match field {
        GeneralSettingsField::BrowserPath => (
            editor.draft.browser_status.clone(),
            editor.draft.browser_status_is_error,
        ),
        GeneralSettingsField::CustomPlayerPath => (
            editor.draft.custom_player_status.clone(),
            editor.draft.custom_player_status_is_error,
        ),
        GeneralSettingsField::StreamlinkPath => (
            editor.draft.streamlink_status.clone(),
            editor.draft.streamlink_status_is_error,
        ),
        GeneralSettingsField::StreamlinkCustomQuality => (
            editor.draft.streamlink_custom_quality_status.clone(),
            editor.draft.streamlink_custom_quality_status_is_error,
        ),
        GeneralSettingsField::StreamlinkExtraArgs => (
            editor.draft.streamlink_extra_args_status.clone(),
            editor.draft.streamlink_extra_args_status_is_error,
        ),
    };
    let default_value = match field {
        GeneralSettingsField::BrowserPath => {
            settings_defaults().browser_path.clone().unwrap_or_default()
        }
        GeneralSettingsField::CustomPlayerPath => settings_defaults()
            .custom_player_path
            .clone()
            .unwrap_or_default(),
        GeneralSettingsField::StreamlinkPath => settings_defaults()
            .streamlink_path
            .clone()
            .unwrap_or_default(),
        GeneralSettingsField::StreamlinkCustomQuality => {
            settings_defaults().streamlink_custom_quality.clone()
        }
        GeneralSettingsField::StreamlinkExtraArgs => {
            settings_defaults().streamlink_extra_args.clone()
        }
    };
    let modified = general_field_value(&editor.draft, field) != default_value;
    setting_target(
        descriptor,
        settings_group_contents(
            format!("players-{}", descriptor.control_id),
            descriptor.label,
            false,
            div()
                .w_full()
                .min_w_0()
                .flex()
                .flex_col()
                .gap_2()
                .child(
                    div()
                        .text_size(px(SettingsMetrics::CALM_LABEL_TEXT))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(colors::text_primary())
                        .child(descriptor.label),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(colors::text_secondary())
                        .child(help),
                )
                .child(render_general_input(field, descriptor.label, window, cx))
                .child(
                    div()
                        .flex()
                        .flex_wrap()
                        .gap_2()
                        .child(settings_button_keyed(
                            format!("players-{}-save", descriptor.control_id),
                            save_label,
                            false,
                            cx,
                            move |cx| {
                                let generation = general_settings_editor_snapshot().generation;
                                save_general_settings_field(generation, field);
                                cx.notify();
                            },
                        ))
                        .when(!paste_label.is_empty(), |actions| {
                            actions.child(settings_button_keyed(
                                format!("players-{}-paste", descriptor.control_id),
                                paste_label,
                                false,
                                cx,
                                move |cx| {
                                    if let Some(text) = crate::utils::read_text(cx) {
                                        paste_general_editor_field(field, text);
                                    }
                                    cx.notify();
                                },
                            ))
                        })
                        .child(settings_button_keyed(
                            format!("players-{}-clear", descriptor.control_id),
                            clear_label,
                            false,
                            cx,
                            move |cx| {
                                clear_general_editor_field(field);
                                cx.notify();
                            },
                        ))
                        .when(modified, |actions| {
                            actions.child(setting_reset(
                                format!("reset-{}", descriptor.control_id),
                                descriptor.label,
                                cx,
                                move |cx| {
                                    reset_general_editor_field(field);
                                    cx.notify();
                                },
                            ))
                        }),
                )
                .when_some(status, |body, status| {
                    body.child(
                        div()
                            .text_xs()
                            .text_color(if status_is_error {
                                colors::signal_red()
                            } else {
                                colors::text_muted()
                            })
                            .child(status),
                    )
                }),
        ),
    )
}

fn render_streamlink_quality_row<T: 'static>(
    current: StreamlinkQuality,
    select: &SettingsSelectState,
    default: StreamlinkQuality,
    stacked: bool,
    cx: &mut Context<T>,
) -> impl IntoElement + use<T> {
    let options = [
        SelectOption::new("best", "Best"),
        SelectOption::new("720p60", "720p60"),
        SelectOption::new("480p", "480p"),
        SelectOption::new("audio", "Audio only"),
        SelectOption::new("custom", "Custom"),
    ];
    setting_target(
        STREAMLINK_QUALITY,
        setting_row(
            STREAMLINK_QUALITY.id,
            STREAMLINK_QUALITY.label,
            Some("Quality passed to Streamlink from menus and commands."),
            settings_select(
                STREAMLINK_QUALITY.control_id,
                STREAMLINK_QUALITY.label,
                &options,
                streamlink_quality_key(current),
                select,
                cx,
                |value, cx| {
                    let quality = match value.as_str() {
                        "720p60" => StreamlinkQuality::P720_60,
                        "480p" => StreamlinkQuality::P480,
                        "audio" => StreamlinkQuality::AudioOnly,
                        "custom" => StreamlinkQuality::Custom,
                        _ => StreamlinkQuality::Best,
                    };
                    crate::backend::settings::set_streamlink_quality(quality);
                    cx.notify();
                },
            ),
            current != default,
            (current != default).then(|| {
                setting_reset(
                    "reset-streamlink-quality",
                    STREAMLINK_QUALITY.label,
                    cx,
                    move |cx| {
                        crate::backend::settings::set_streamlink_quality(default);
                        cx.notify();
                    },
                )
            }),
            stacked,
        ),
    )
}

fn normalize_general_editor_text(
    field: GeneralSettingsField,
    text: &str,
) -> Result<String, &'static str> {
    match field {
        GeneralSettingsField::StreamlinkExtraArgs => Ok(text
            .replace("\r\n", " ")
            .replace(['\r', '\n'], " ")
            .to_string()),
        _ if text.contains('\r') || text.contains('\n') => Err("This path must be one line."),
        _ => Ok(text.to_string()),
    }
}

#[cfg(test)]
mod player_input_text_tests {
    use super::{GeneralSettingsField, normalize_general_editor_text};
    use core::prelude::v1::test;

    #[test]
    fn typing_keeps_spaces_until_checked_save() {
        assert_eq!(
            normalize_general_editor_text(
                GeneralSettingsField::StreamlinkExtraArgs,
                "--retry-streams "
            ),
            Ok("--retry-streams ".to_string())
        );
        assert_eq!(
            normalize_general_editor_text(GeneralSettingsField::CustomPlayerPath, "C:\\Program "),
            Ok("C:\\Program ".to_string())
        );
    }

    #[test]
    fn paths_reject_multiline_paste_but_arguments_keep_a_separator() {
        assert_eq!(
            normalize_general_editor_text(GeneralSettingsField::StreamlinkPath, "one\ntwo"),
            Err("This path must be one line.")
        );
        assert_eq!(
            normalize_general_editor_text(
                GeneralSettingsField::StreamlinkExtraArgs,
                "--retry-streams 5\r\n--player-args x",
            ),
            Ok("--retry-streams 5 --player-args x".to_string())
        );
    }
}

fn set_general_editor_field_text(field: GeneralSettingsField, text: String) {
    let value = normalize_general_editor_text(field, &text)
        .unwrap_or_else(|_| single_line_text(&text).trim().to_string());
    let authority = general_field_authority(field);
    update_general_settings_editor(|editor| {
        editor.dirty[general_field_index(field)] = value != authority;
        let (target, status, status_is_error) = general_editor_field_parts(editor, field);
        *target = value;
        *status = None;
        *status_is_error = false;
    });
}

fn paste_general_editor_field(field: GeneralSettingsField, text: String) {
    let value = match normalize_general_editor_text(field, &text) {
        Ok(value) => value,
        Err(message) => {
            update_general_settings_editor(|editor| {
                let (_, status, status_is_error) = general_editor_field_parts(editor, field);
                *status = Some(message.to_string());
                *status_is_error = true;
            });
            return;
        }
    };
    set_general_editor_field_text(field, value);
    let snapshot = general_settings_editor_snapshot();
    if snapshot.draft.dirty[general_field_index(field)] {
        save_general_settings_field(snapshot.generation, field);
    }
}

fn revert_general_editor_field(field: GeneralSettingsField) {
    let authority = general_field_authority(field);
    apply_general_authoritative_update(|editor| {
        let (target, status, status_is_error) = general_editor_field_parts(editor, field);
        *target = authority;
        *status = None;
        *status_is_error = false;
        editor.dirty[general_field_index(field)] = false;
        if editor.focused == Some(field) {
            editor.focused = None;
        }
    });
}

fn clear_general_editor_field(field: GeneralSettingsField) {
    match field {
        GeneralSettingsField::BrowserPath => crate::backend::settings::set_browser_path(None),
        GeneralSettingsField::CustomPlayerPath => {
            crate::backend::settings::set_custom_player_path(None)
        }
        GeneralSettingsField::StreamlinkPath => crate::backend::settings::set_streamlink_path(None),
        GeneralSettingsField::StreamlinkCustomQuality => {
            crate::backend::settings::set_streamlink_custom_quality(String::new())
        }
        GeneralSettingsField::StreamlinkExtraArgs => {
            crate::backend::settings::set_streamlink_extra_args(String::new())
        }
    }
    apply_general_authoritative_update(|editor| {
        let (target, status, status_is_error) = general_editor_field_parts(editor, field);
        target.clear();
        *status = Some(
            match field {
                GeneralSettingsField::BrowserPath => {
                    "Using automatic private-window browser selection"
                }
                GeneralSettingsField::CustomPlayerPath => "No custom player executable configured",
                GeneralSettingsField::StreamlinkPath => "Using streamlink from PATH",
                GeneralSettingsField::StreamlinkCustomQuality => {
                    "Custom quality will fall back to best"
                }
                GeneralSettingsField::StreamlinkExtraArgs => "No Streamlink extra arguments",
            }
            .to_string(),
        );
        *status_is_error = false;
        editor.dirty[general_field_index(field)] = false;
        editor.focused = None;
        if field == GeneralSettingsField::BrowserPath {
            editor.browser_path_mode = false;
        }
    });
}

fn reset_general_editor_field(field: GeneralSettingsField) {
    match field {
        GeneralSettingsField::BrowserPath => crate::backend::settings::set_browser_path(None),
        GeneralSettingsField::CustomPlayerPath => {
            crate::backend::settings::set_custom_player_path(None)
        }
        GeneralSettingsField::StreamlinkPath => crate::backend::settings::set_streamlink_path(None),
        GeneralSettingsField::StreamlinkCustomQuality => {
            crate::backend::settings::set_streamlink_custom_quality(
                settings_defaults().streamlink_custom_quality.clone(),
            )
        }
        GeneralSettingsField::StreamlinkExtraArgs => {
            crate::backend::settings::set_streamlink_extra_args(
                settings_defaults().streamlink_extra_args.clone(),
            )
        }
    }
    let authority = general_field_authority(field);
    apply_general_authoritative_update(|editor| {
        let (target, status, status_is_error) = general_editor_field_parts(editor, field);
        *target = authority;
        *status = None;
        *status_is_error = false;
        editor.dirty[general_field_index(field)] = false;
        editor.focused = None;
        if field == GeneralSettingsField::BrowserPath {
            editor.browser_path_mode = false;
        }
    });
}

fn render_general_editor_recovery_status() -> impl IntoElement {
    div()
        .text_xs()
        .text_color(colors::signal_red())
        .child(
            "Player/path drafts were reset after an internal editor error. Review or edit them before saving.",
        )
}

fn streamlink_quality_key(quality: StreamlinkQuality) -> &'static str {
    match quality {
        StreamlinkQuality::Best => "best",
        StreamlinkQuality::P720_60 => "720p60",
        StreamlinkQuality::P480 => "480p",
        StreamlinkQuality::AudioOnly => "audio",
        StreamlinkQuality::Custom => "custom",
    }
}

/// Transient, settings-only state for the timestamp token builder.
///
/// Global for the same reason as [`GENERAL_SETTINGS_EDITOR`]: the section
/// renderers are free functions shared by the settings modal and the native
/// popout, so there is no per-surface entity to hang editing state on. Only
/// editing affordances live here; the pattern itself is persisted settings.
#[derive(Default)]
struct TimestampBuilderState {
    /// Chip whose property editor is expanded.
    selected: Option<usize>,
    /// Whether the grouped "Add a piece" menu is open.
    add_menu_open: bool,
    /// Whether the custom-separator field owns settings text input.
    literal_focused: bool,
    /// Draft text for the custom separator.
    literal_draft: String,
    /// Last add/reorder/validation message.
    status: Option<String>,
    status_is_error: bool,
    /// Whether a preview refresh timer is already in flight.
    preview_pending: bool,
    /// Formatter generation the in-flight timer was armed for.
    preview_generation: u64,
}

static TIMESTAMP_BUILDER: LazyLock<Mutex<TimestampBuilderState>> =
    LazyLock::new(|| Mutex::new(TimestampBuilderState::default()));

fn recovered_timestamp_builder_state() -> TimestampBuilderState {
    TimestampBuilderState {
        status: Some(
            "Timestamp builder state was reset after an internal editor error. Select or edit a piece again."
                .to_string(),
        ),
        status_is_error: true,
        ..TimestampBuilderState::default()
    }
}

fn lock_timestamp_builder_state(
    mutex: &Mutex<TimestampBuilderState>,
) -> (MutexGuard<'_, TimestampBuilderState>, bool) {
    match mutex.lock() {
        Ok(state) => (state, false),
        Err(poisoned) => {
            let mut state = poisoned.into_inner();
            *state = recovered_timestamp_builder_state();
            mutex.clear_poison();
            crate::log_ui!(warn, "Reset poisoned timestamp builder state");
            (state, true)
        }
    }
}

fn update_timestamp_builder(update: impl FnOnce(&mut TimestampBuilderState)) -> bool {
    let (mut state, recovered) = lock_timestamp_builder_state(&TIMESTAMP_BUILDER);
    if recovered {
        return false;
    }
    update(&mut state);
    true
}

fn apply_timestamp_builder_authoritative(update: impl FnOnce(&mut TimestampBuilderState)) {
    let (mut state, _) = lock_timestamp_builder_state(&TIMESTAMP_BUILDER);
    update(&mut state);
}

pub(super) fn reset_timestamp_builder_after_settings_replace() {
    let (mut state, _) = lock_timestamp_builder_state(&TIMESTAMP_BUILDER);
    *state = TimestampBuilderState {
        status: Some("Timestamp builder reset after settings were replaced".to_string()),
        ..TimestampBuilderState::default()
    };
}

/// One render pass's view of the builder's transient state.
#[derive(Default)]
struct TimestampBuilderView {
    selected: Option<usize>,
    add_menu_open: bool,
    literal_focused: bool,
    literal_draft: String,
    status: Option<String>,
    status_is_error: bool,
}

/// Snapshot the builder's transient state, releasing the lock before render.
fn timestamp_builder_snapshot() -> TimestampBuilderView {
    let (state, _) = lock_timestamp_builder_state(&TIMESTAMP_BUILDER);
    TimestampBuilderView {
        selected: state.selected,
        add_menu_open: state.add_menu_open,
        literal_focused: state.literal_focused,
        literal_draft: state.literal_draft.clone(),
        status: state.status.clone(),
        status_is_error: state.status_is_error,
    }
}

/// Persist an edited token list, keeping the builder's selection in sync.
///
/// Validation lives in the backend so the settings UI can never persist a
/// pattern the compiled formatter would have to guess about; a rejection is
/// surfaced as inline status text instead of silently doing nothing.
fn commit_timestamp_tokens<T: 'static>(
    tokens: Vec<TimestampToken>,
    select: Option<usize>,
    cx: &mut Context<T>,
) {
    let pattern = TimestampPattern {
        version: TIMESTAMP_PATTERN_VERSION,
        tokens,
    };
    match crate::backend::settings::set_message_timestamp_pattern(pattern) {
        Ok(()) => {
            apply_timestamp_builder_authoritative(|state| {
                state.selected = select;
                state.status = None;
                state.status_is_error = false;
            });
        }
        Err(err) => {
            apply_timestamp_builder_authoritative(|state| {
                state.status = Some(err.message().to_string());
                state.status_is_error = true;
            });
        }
    }
    cx.notify();
}

/// Keep the live preview fresh without adding a per-frame wakeup.
///
/// Exactly one timer may be in flight, and it is only re-armed while the
/// builder is actually rendering. Closing Settings or hiding timestamps
/// therefore stops the loop after at most one more tick. Millisecond patterns
/// are capped at 10 Hz by [`TimestampResolution::preview_interval_ms`].
fn arm_timestamp_preview_refresh<T: 'static>(
    resolution: TimestampResolution,
    generation: u64,
    cx: &mut Context<T>,
) {
    let should_arm = {
        let (mut state, _) = lock_timestamp_builder_state(&TIMESTAMP_BUILDER);
        mark_timestamp_preview_pending(&mut state, generation)
    };
    if !should_arm {
        return;
    }

    let interval = std::time::Duration::from_millis(resolution.preview_interval_ms());
    cx.spawn(async move |this, cx| {
        cx.background_executor().timer(interval).await;
        finish_timestamp_preview_timer(generation);
        let _ = this.update(cx, |_this, cx| {
            crate::backend::metrics::record_ui_notify_source("settings.timestamp.preview");
            cx.notify();
        });
    })
    .detach();
}

fn mark_timestamp_preview_pending(state: &mut TimestampBuilderState, generation: u64) -> bool {
    if state.preview_pending && state.preview_generation == generation {
        return false;
    }
    state.preview_pending = true;
    state.preview_generation = generation;
    true
}

fn finish_timestamp_preview_timer(generation: u64) {
    let (mut state, recovered) = lock_timestamp_builder_state(&TIMESTAMP_BUILDER);
    if !recovered {
        finish_timestamp_preview_timer_state(&mut state, generation);
    }
}

fn finish_timestamp_preview_timer_state(state: &mut TimestampBuilderState, generation: u64) {
    if state.preview_pending && state.preview_generation == generation {
        state.preview_pending = false;
    }
}

#[cfg(test)]
mod timestamp_builder_state_tests {
    use super::*;
    use core::prelude::v1::test;

    #[test]
    fn recovery_discards_transient_builder_state() {
        let recovered = recovered_timestamp_builder_state();

        assert!(recovered.selected.is_none());
        assert!(!recovered.add_menu_open);
        assert!(!recovered.literal_focused);
        assert!(recovered.literal_draft.is_empty());
        assert!(recovered.status_is_error);
        assert!(recovered.status.is_some());
        assert!(!recovered.preview_pending);
        assert_eq!(recovered.preview_generation, 0);
    }

    #[test]
    fn preview_timer_is_single_flight_per_generation() {
        let mut state = TimestampBuilderState::default();

        assert!(mark_timestamp_preview_pending(&mut state, 7));
        assert!(!mark_timestamp_preview_pending(&mut state, 7));
        assert!(mark_timestamp_preview_pending(&mut state, 8));
        assert!(state.preview_pending);
        assert_eq!(state.preview_generation, 8);
    }

    #[test]
    fn prior_generation_timer_cannot_finish_current_timer() {
        let mut state = TimestampBuilderState {
            preview_pending: true,
            preview_generation: 8,
            ..TimestampBuilderState::default()
        };

        finish_timestamp_preview_timer_state(&mut state, 7);
        assert!(state.preview_pending);

        finish_timestamp_preview_timer_state(&mut state, 8);
        assert!(!state.preview_pending);
    }
}

/// Route key input to the timestamp builder. Returns whether it consumed one.
///
/// Left/Right reorder the selected chip and Delete removes it, matching the
/// pointer affordances on the chip itself. Keys typed in the custom-separator
/// field belong to that text input.
fn handle_timestamp_builder_key<T: 'static>(event: &KeyDownEvent, cx: &mut Context<T>) -> bool {
    let view = timestamp_builder_snapshot();

    if view.literal_focused {
        return false;
    }

    if view.add_menu_open && event.keystroke.key == "escape" {
        update_timestamp_builder(|state| {
            state.add_menu_open = false;
            state.literal_focused = false;
        });
        return true;
    }

    let Some(index) = view.selected else {
        return false;
    };
    let pattern = crate::backend::settings::get_message_timestamp_pattern();
    if index >= pattern.tokens.len() {
        update_timestamp_builder(|state| state.selected = None);
        return true;
    }

    match event.keystroke.key.as_str() {
        "left" if index > 0 => {
            let mut tokens = pattern.tokens;
            tokens.swap(index, index - 1);
            commit_timestamp_tokens(tokens, Some(index - 1), cx);
            true
        }
        "right" if index + 1 < pattern.tokens.len() => {
            let mut tokens = pattern.tokens;
            tokens.swap(index, index + 1);
            commit_timestamp_tokens(tokens, Some(index + 1), cx);
            true
        }
        "delete" | "backspace" if pattern.tokens.len() > 1 => {
            let mut tokens = pattern.tokens;
            tokens.remove(index);
            commit_timestamp_tokens(tokens, None, cx);
            true
        }
        "escape" => {
            update_timestamp_builder(|state| state.selected = None);
            true
        }
        _ => false,
    }
}

/// Append draft text, enforcing the visible-character limit up front so the
/// field can never hold something the backend would reject on submit.
fn push_timestamp_literal_draft(state: &mut TimestampBuilderState, text: &str) {
    for ch in text.chars() {
        if state.literal_draft.chars().count() >= MAX_TIMESTAMP_LITERAL_CHARS {
            break;
        }
        state.literal_draft.push(ch);
    }
    state.status = None;
    state.status_is_error = false;
}

fn render_timestamp_builder<T: 'static>(
    pattern: TimestampPattern,
    literal_preset_select: SettingsSelectState,
    literal_input: DraftField<T>,
    cx: &mut Context<T>,
) -> impl IntoElement + use<T> {
    let formatter = crate::backend::settings::message_timestamp_formatter();
    let preview = crate::utils::timestamp::format_message_timestamp_with_formatter(
        chrono::Local::now().timestamp_millis(),
        formatter.clone(),
    )
    .unwrap_or_else(|| "--:--".to_string());
    arm_timestamp_preview_refresh(formatter.resolution, formatter.generation, cx);

    let TimestampBuilderView {
        selected,
        add_menu_open,
        literal_draft,
        status,
        status_is_error,
        ..
    } = timestamp_builder_snapshot();
    let selected = selected.filter(|index| *index < pattern.tokens.len());

    div()
        .w_full()
        .flex()
        .flex_col()
        .gap_2()
        .child(render_timestamp_preview(&preview))
        .child(render_timestamp_chips(&pattern, selected, cx))
        .when_some(selected, |el, index| {
            el.child(render_timestamp_token_properties(
                &pattern,
                index,
                &literal_preset_select,
                cx,
            ))
        })
        .child(render_timestamp_add_menu(
            &pattern,
            add_menu_open,
            &literal_draft,
            literal_input,
            cx,
        ))
        .when_some(status, |el, message| {
            el.child(
                div()
                    .text_xs()
                    .text_color(if status_is_error {
                        colors::error()
                    } else {
                        colors::text_muted()
                    })
                    .child(message),
            )
        })
        .child(
            div()
                .flex()
                .child(render_button("Reset timestamp format", false, cx, |cx| {
                    commit_timestamp_tokens(TimestampPattern::default().tokens, None, cx);
                })),
        )
}

fn render_timestamp_preview(preview: &str) -> impl IntoElement + use<> {
    div()
        .w_full()
        .flex()
        .items_center()
        .gap_2()
        .child(
            div()
                .text_xs()
                .text_color(colors::text_muted())
                .child("Preview"),
        )
        .child(
            div()
                .px_2()
                .py_1()
                .rounded(Radii::sm())
                .bg(colors::bg_surface())
                .border_1()
                .border_color(colors::border())
                .text_sm()
                .font_weight(FontWeight::MEDIUM)
                .text_color(colors::text_primary())
                .child(preview.to_string()),
        )
}

/// Ordered chips with per-chip reorder and remove affordances.
fn render_timestamp_chips<T: 'static>(
    pattern: &TimestampPattern,
    selected: Option<usize>,
    cx: &mut Context<T>,
) -> impl IntoElement + use<T> {
    let tokens = pattern.tokens.clone();
    let count = tokens.len();
    let names: Vec<String> = tokens.iter().map(TimestampToken::accessible_name).collect();

    div()
        .w_full()
        .flex()
        .flex_wrap()
        .items_center()
        .gap_1()
        .children(tokens.iter().enumerate().map(|(index, token)| {
            let is_selected = selected == Some(index);
            let tokens_for_left = tokens.clone();
            let tokens_for_right = tokens.clone();
            let tokens_for_remove = tokens.clone();
            let move_left = (index > 0).then(|| {
                render_timestamp_chip_action(
                    format!("timestamp-chip-left-{index}"),
                    "\u{25c0}",
                    format!(
                        "Move {} before {}",
                        names[index],
                        names[index.saturating_sub(1)]
                    ),
                    cx,
                    move |cx| {
                        let mut tokens = tokens_for_left.clone();
                        tokens.swap(index, index - 1);
                        commit_timestamp_tokens(tokens, Some(index - 1), cx);
                    },
                )
            });
            let move_right = (index + 1 < count).then(|| {
                render_timestamp_chip_action(
                    format!("timestamp-chip-right-{index}"),
                    "\u{25b6}",
                    format!("Move {} after {}", names[index], names[index + 1]),
                    cx,
                    move |cx| {
                        let mut tokens = tokens_for_right.clone();
                        tokens.swap(index, index + 1);
                        commit_timestamp_tokens(tokens, Some(index + 1), cx);
                    },
                )
            });
            let remove = (count > 1).then(|| {
                render_timestamp_chip_action(
                    format!("timestamp-chip-remove-{index}"),
                    "\u{2715}",
                    format!("Remove {}", names[index]),
                    cx,
                    move |cx| {
                        let mut tokens = tokens_for_remove.clone();
                        tokens.remove(index);
                        commit_timestamp_tokens(tokens, None, cx);
                    },
                )
            });

            div()
                .id(SharedString::from(format!("timestamp-chip-{index}")))
                .role(Role::Button)
                .map(|control| {
                    crate::components::settings::widgets::settings_keyboard_control(control, cx)
                })
                .a11y_synthetic_children(crate::components::ui::accessibility::apply_description)
                .aria_label(SharedString::from(names[index].clone()))
                .aria_selected(is_selected)
                .flex()
                .items_center()
                .gap_1()
                .pl_2()
                .pr_1()
                .py_0p5()
                .min_h(px(28.0))
                .rounded(Radii::sm())
                .bg(if is_selected {
                    colors::bg_hover()
                } else {
                    colors::bg_surface()
                })
                .border_1()
                .border_color(if is_selected {
                    colors::accent()
                } else {
                    colors::border()
                })
                .cursor_pointer()
                .on_activate(cx.listener(move |_this, _event, _window, cx| {
                    update_timestamp_builder(|state| {
                        state.selected = if state.selected == Some(index) {
                            None
                        } else {
                            Some(index)
                        };
                    });
                    cx.notify();
                }))
                .on_a11y_action(AccessibleAction::Click, {
                    let entity = cx.entity();
                    move |_, _, app| {
                        entity.update(app, |_, cx| {
                            update_timestamp_builder(|state| {
                                state.selected = if state.selected == Some(index) {
                                    None
                                } else {
                                    Some(index)
                                };
                            });
                            cx.notify();
                        })
                    }
                })
                .child(
                    div()
                        .text_xs()
                        .text_color(colors::text_primary())
                        .child(token.chip_label()),
                )
                .when_some(move_left, |el, action| el.child(action))
                .when_some(move_right, |el, action| el.child(action))
                .when_some(remove, |el, action| el.child(action))
        }))
}

/// One 20px glyph button inside a chip, carrying its own accessible label.
fn render_timestamp_chip_action<T: 'static, F: Fn(&mut Context<T>) + 'static>(
    id: String,
    glyph: &'static str,
    label: String,
    cx: &mut Context<T>,
    on_click: F,
) -> impl IntoElement + use<T, F> {
    let on_click = std::rc::Rc::new(on_click);
    let pointer = on_click.clone();
    let entity = cx.entity();
    div()
        .id(SharedString::from(id))
        .role(Role::Button)
        .map(|control| crate::components::settings::widgets::settings_keyboard_control(control, cx))
        .a11y_synthetic_children(crate::components::ui::accessibility::apply_description)
        .aria_label(SharedString::from(label))
        .flex()
        .items_center()
        .justify_center()
        .w(px(20.0))
        .h(px(20.0))
        .rounded(Radii::sm())
        .cursor_pointer()
        .text_xs()
        .text_color(colors::text_muted())
        .hover(|s| {
            s.bg(colors::hover_surface())
                .text_color(colors::text_primary())
        })
        .on_activate(cx.listener(move |_this, _event, _window, cx| {
            cx.stop_propagation();
            pointer(cx);
        }))
        .on_a11y_action(AccessibleAction::Click, move |_, _, app| {
            entity.update(app, |_, cx| on_click(cx));
        })
        .child(glyph)
}

/// Property editor for the selected chip.
fn render_timestamp_token_properties<T: 'static>(
    pattern: &TimestampPattern,
    index: usize,
    literal_preset_select: &SettingsSelectState,
    cx: &mut Context<T>,
) -> impl IntoElement + use<T> {
    let tokens = pattern.tokens.clone();
    let token = tokens[index].clone();

    let body = match token {
        TimestampToken::Hour24 { padded } | TimestampToken::Hour12 { padded } => {
            let twelve_hour = matches!(tokens[index], TimestampToken::Hour12 { .. });
            let tokens_for_change = tokens.clone();
            select_buttons(
                "timestamp-hour-padding",
                &[
                    SelectOption::new("compact", "9"),
                    SelectOption::new("padded", "09"),
                ],
                if padded { "padded" } else { "compact" },
                cx,
                move |value, cx| {
                    let mut tokens = tokens_for_change.clone();
                    let padded = value == "padded";
                    tokens[index] = if twelve_hour {
                        TimestampToken::Hour12 { padded }
                    } else {
                        TimestampToken::Hour24 { padded }
                    };
                    commit_timestamp_tokens(tokens, Some(index), cx);
                },
            )
            .into_any_element()
        }
        TimestampToken::Meridiem { uppercase } => {
            let tokens_for_change = tokens.clone();
            select_buttons(
                "timestamp-meridiem-case",
                &[
                    SelectOption::new("lower", "am/pm"),
                    SelectOption::new("upper", "AM/PM"),
                ],
                if uppercase { "upper" } else { "lower" },
                cx,
                move |value, cx| {
                    let mut tokens = tokens_for_change.clone();
                    tokens[index] = TimestampToken::Meridiem {
                        uppercase: value == "upper",
                    };
                    commit_timestamp_tokens(tokens, Some(index), cx);
                },
            )
            .into_any_element()
        }
        TimestampToken::Fraction { digits } => {
            let tokens_for_change = tokens.clone();
            select_buttons(
                "timestamp-fraction-digits",
                &[
                    SelectOption::new("1", "1 digit"),
                    SelectOption::new("2", "2 digits"),
                    SelectOption::new("3", "3 digits"),
                ],
                &digits.to_string(),
                cx,
                move |value, cx| {
                    let mut tokens = tokens_for_change.clone();
                    tokens[index] = TimestampToken::Fraction {
                        digits: value.parse().unwrap_or(3),
                    };
                    commit_timestamp_tokens(tokens, Some(index), cx);
                },
            )
            .into_any_element()
        }
        TimestampToken::Literal { ref text } => {
            let tokens_for_change = tokens.clone();
            let options: Vec<SelectOption> = TIMESTAMP_LITERAL_PRESETS
                .iter()
                .map(|(value, label)| SelectOption::new(*value, *label))
                .collect();
            let current = text.clone();
            div()
                .on_key_down(cx.listener(|_this, event: &KeyDownEvent, _window, cx| {
                    if matches!(
                        event.keystroke.key.as_str(),
                        "left" | "right" | "backspace" | "delete"
                    ) {
                        cx.stop_propagation();
                    }
                }))
                .child(settings_select(
                    "timestamp-literal-preset",
                    "Separator preset",
                    &options,
                    &current,
                    literal_preset_select,
                    cx,
                    move |value, cx| {
                        let mut tokens = tokens_for_change.clone();
                        tokens[index] = TimestampToken::literal(&value);
                        commit_timestamp_tokens(tokens, Some(index), cx);
                    },
                ))
                .into_any_element()
        }
        TimestampToken::Minute | TimestampToken::Second => div()
            .text_xs()
            .text_color(colors::text_muted())
            .child("This piece has no options.")
            .into_any_element(),
    };

    div()
        .w_full()
        .flex()
        .flex_col()
        .gap_1()
        .p_2()
        .rounded(Radii::sm())
        .bg(colors::bg_surface())
        .border_1()
        .border_color(colors::border())
        .child(
            div()
                .text_xs()
                .text_color(colors::text_muted())
                .child(format!("{} options", tokens[index].accessible_name())),
        )
        .child(body)
}

/// Grouped add menu. Entries that would produce an invalid pattern are
/// rendered disabled with their reason, so the menu can never offer an
/// addition the backend would reject.
fn render_timestamp_add_menu<T: 'static>(
    pattern: &TimestampPattern,
    is_open: bool,
    literal_draft: &str,
    literal_input: DraftField<T>,
    cx: &mut Context<T>,
) -> impl IntoElement + use<T> {
    let time_pieces: Vec<TimestampToken> = vec![
        TimestampToken::Hour24 { padded: false },
        TimestampToken::Hour12 { padded: false },
        TimestampToken::Minute,
        TimestampToken::Second,
        TimestampToken::Meridiem { uppercase: false },
    ];
    let separators: Vec<TimestampToken> = TIMESTAMP_LITERAL_PRESETS
        .iter()
        .map(|(value, _)| TimestampToken::literal(value))
        .collect();
    let separator_labels: Vec<&str> = TIMESTAMP_LITERAL_PRESETS
        .iter()
        .map(|(_, label)| *label)
        .collect();
    let advanced: Vec<TimestampToken> = vec![
        TimestampToken::Fraction { digits: 1 },
        TimestampToken::Fraction { digits: 2 },
        TimestampToken::Fraction { digits: 3 },
    ];
    let advanced_labels = ["Fraction 1 digit", "Fraction 2 digits", "Fraction 3 digits"];
    let draft = literal_draft.to_string();
    let draft_for_add = draft.clone();

    div()
        .w_full()
        .flex()
        .flex_col()
        .gap_2()
        .child(
            div()
                .id("timestamp-add-menu-toggle")
                .role(Role::Button)
                .map(|control| {
                    crate::components::settings::widgets::settings_keyboard_control(control, cx)
                })
                .a11y_synthetic_children(crate::components::ui::accessibility::apply_description)
                .aria_label("Add a timestamp piece")
                .aria_expanded(is_open)
                .flex()
                .items_center()
                .justify_center()
                .self_start()
                .px_2()
                .min_h(px(28.0))
                .rounded(Radii::sm())
                .bg(colors::bg_surface())
                .border_1()
                .border_color(colors::border())
                .cursor_pointer()
                .hover(|s| s.bg(colors::bg_hover()))
                .text_xs()
                .text_color(colors::interactive_foreground())
                .tab_index(0)
                .on_accessible_activate(cx.listener(move |_this, _event, _window, cx| {
                    update_timestamp_builder(|state| {
                        state.add_menu_open = !state.add_menu_open;
                        if !state.add_menu_open {
                            state.literal_focused = false;
                        }
                    });
                    cx.notify();
                }))
                .child(if is_open { "Close" } else { "Add a piece" }),
        )
        .when(is_open, |el| {
            el.child(
                menu_panel()
                    .w_full()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .p_2()
                    .child(render_timestamp_add_group(
                        "Time pieces",
                        pattern,
                        &time_pieces,
                        &time_pieces
                            .iter()
                            .map(|token| token.kind().label().to_string())
                            .collect::<Vec<_>>(),
                        "time",
                        cx,
                    ))
                    .child(render_timestamp_add_group(
                        "Separators",
                        pattern,
                        &separators,
                        &separator_labels
                            .iter()
                            .map(|label| (*label).to_string())
                            .collect::<Vec<_>>(),
                        "separator",
                        cx,
                    ))
                    .child(render_timestamp_add_group(
                        "Advanced",
                        pattern,
                        &advanced,
                        &advanced_labels
                            .iter()
                            .map(|label| (*label).to_string())
                            .collect::<Vec<_>>(),
                        "advanced",
                        cx,
                    ))
                    .child(
                        div()
                            .w_full()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(
                                div()
                                    .text_xs()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(colors::text_secondary())
                                    .child("Custom separator"),
                            )
                            .child(literal_input.render(cx))
                            .child(
                                div()
                                    .w_full()
                                    .min_w_0()
                                    .flex()
                                    .flex_wrap()
                                    .gap_1()
                                    .child(render_button("Paste separator", false, cx, |cx| {
                                        let pasted =
                                            crate::utils::read_text(cx).unwrap_or_default();
                                        update_timestamp_builder(|state| {
                                            state.literal_draft.clear();
                                            push_timestamp_literal_draft(state, &pasted);
                                        });
                                        cx.notify();
                                    }))
                                    .child(render_button("Add separator", false, cx, move |cx| {
                                        add_custom_timestamp_literal(&draft_for_add, cx);
                                    })),
                            ),
                    ),
            )
        })
}

/// One labeled group of add-menu entries.
fn render_timestamp_add_group<T: 'static>(
    title: &str,
    pattern: &TimestampPattern,
    tokens: &[TimestampToken],
    labels: &[String],
    id_prefix: &str,
    cx: &mut Context<T>,
) -> impl IntoElement + use<T> {
    let entries: Vec<(TimestampToken, String, Option<&'static str>)> = tokens
        .iter()
        .enumerate()
        .map(|(position, token)| {
            let reason = pattern.can_append(token).err().map(|err| err.message());
            (
                token.clone(),
                labels
                    .get(position)
                    .cloned()
                    .unwrap_or_else(|| token.kind().label().to_string()),
                reason,
            )
        })
        .collect();
    let prefix = id_prefix.to_string();

    div()
        .w_full()
        .flex()
        .flex_col()
        .gap_1()
        .child(
            div()
                .text_xs()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(colors::text_secondary())
                .child(title.to_string()),
        )
        .child(
            div()
                .flex()
                .flex_wrap()
                .gap_1()
                .children(entries.into_iter().enumerate().map(
                    |(position, (token, label, reason))| {
                        let disabled = reason.is_some();
                        let mut element = div()
                            .id(SharedString::from(format!(
                                "timestamp-add-{prefix}-{position}"
                            )))
                            .role(Role::Button)
                            .map(|control| {
                                crate::components::settings::widgets::settings_keyboard_control_if_enabled(
                                    control, !disabled, cx,
                                )
                            })
                            .a11y_synthetic_children(
                                crate::components::ui::accessibility::apply_description,
                            )
                            .aria_label(SharedString::from(format!("Add {label}")))
                            .flex()
                            .items_center()
                            .px_2()
                            .min_h(px(28.0))
                            .rounded(Radii::sm())
                            .bg(colors::bg_elevated())
                            .border_1()
                            .border_color(colors::border())
                            .text_xs()
                            .text_color(if disabled {
                                colors::text_muted()
                            } else {
                                colors::interactive_foreground()
                            })
                            .child(label);

                        if let Some(reason) = reason {
                            element = element
                                .aria_description(SharedString::from(reason))
                                .disabled_button();
                        } else {
                            element = element
                                .cursor_pointer()
                                .hover(button_hover)
                                .on_accessible_activate(cx.listener(
                                    move |_this, _event, _window, cx| {
                                        let mut tokens =
                                        crate::backend::settings::get_message_timestamp_pattern()
                                            .tokens;
                                        tokens.push(token.clone());
                                        let select = tokens.len() - 1;
                                        commit_timestamp_tokens(tokens, Some(select), cx);
                                    },
                                ));
                        }
                        element
                    },
                )),
        )
}

/// Validate and append a custom separator from the builder's draft field.
fn timestamp_literal_input<T: 'static>(draft: &str) -> DraftInput<T> {
    DraftInput::new(
        "timestamp-custom-literal",
        draft,
        "Type or paste up to 8 characters",
    )
    .label("Custom separator")
    .on_focus(|_cx| {
        update_timestamp_builder(|state| state.literal_focused = true);
    })
    .on_blur(|_cx| {
        update_timestamp_builder(|state| state.literal_focused = false);
    })
    .on_change(|text, _cx| {
        update_timestamp_builder(|state| {
            state.literal_draft.clear();
            push_timestamp_literal_draft(state, &text);
        });
    })
    .on_enter(|cx| {
        let draft = timestamp_builder_snapshot().literal_draft;
        add_custom_timestamp_literal(&draft, cx);
    })
    .on_escape(|_window, _cx| {
        update_timestamp_builder(|state| state.literal_draft.clear());
    })
}

fn add_custom_timestamp_literal<T: 'static>(draft: &str, cx: &mut Context<T>) {
    if let Err(err) = validate_timestamp_literal(draft) {
        update_timestamp_builder(|state| {
            state.status = Some(err.message().to_string());
            state.status_is_error = true;
        });
        cx.notify();
        return;
    }

    let mut tokens = crate::backend::settings::get_message_timestamp_pattern().tokens;
    tokens.push(TimestampToken::literal(draft));
    let select = tokens.len() - 1;
    commit_timestamp_tokens(tokens, Some(select), cx);
    update_timestamp_builder(|state| {
        state.literal_draft.clear();
        state.literal_focused = false;
    });
}

fn render_configuration_actions<T: 'static>(
    settings_path: std::path::PathBuf,
    ui: &mut SettingsPageUi,
    window: &mut Window,
    cx: &mut Context<T>,
) -> impl IntoElement + use<T> {
    let path_label = settings_path.display().to_string();
    let path_for_copy = path_label.clone();
    let path_for_open = settings_path.clone();
    let settings_dir = SETTINGS_DIR_PATH.clone();
    let settings_dir_label = settings_dir.display().to_string();
    let backups_dir = SETTINGS_BACKUPS_DIR_PATH.clone();
    let backups_dir_label = backups_dir.display().to_string();
    let stacked = ui.stacked;
    let files_disclosure = ui.disclosure("advanced-configuration-files", cx);
    let editor = settings_import_editor_snapshot();
    let import_disclosure = ui.disclosure("advanced-settings-import", cx);
    let summary = settings_import_summary(&editor);
    if !import_disclosure.is_expanded() && editor.draft.focused {
        SETTINGS_IMPORT_EDITOR.sync_and_snapshot(recovered_settings_import_editor, |editor| {
            editor.focused = false;
            false
        });
    }
    let escape_focus = import_disclosure.clone();
    let chatterino_import = super::chatterino_import::render_chatterino_import(ui, cx);
    div()
        .w_full()
        .min_w_0()
        .flex()
        .flex_col()
        .gap(px(SettingsMetrics::GROUP_GAP))
        .child(settings_group(
            "advanced-configuration",
            "Configuration",
            true,
            div()
                .flex()
                .flex_col()
                .child(setting_target(
                    SETTINGS_EXPORT,
                    crate::components::settings::widgets::setting_compact_row(
                        SETTINGS_EXPORT.id,
                        SETTINGS_EXPORT.label,
                        Some("Copy a settings snapshot with secrets removed."),
                        quiet_action_button(
                            SETTINGS_EXPORT.control_id,
                            "Export",
                            false,
                            cx,
                            |cx| {
                                match crate::backend::settings::export_settings_to_json() {
                                    Ok(json) => {
                                        cx.write_to_clipboard(ClipboardItem::new_string(json));
                                        set_settings_import_status(
                                            "Exported settings JSON and copied it to the clipboard"
                                                .to_string(),
                                            false,
                                        );
                                    }
                                    Err(error) => {
                                        set_settings_import_status(error, true);
                                    }
                                }
                                cx.notify();
                            },
                        )
                        .aria_label("Export settings JSON to clipboard"),
                        false,
                        None,
                        ui.stacked,
                    ),
                ))
                .child(settings_summary_disclosure(
                    "advanced-configuration-files",
                    "Files and folders",
                    "Settings and backups",
                    &files_disclosure,
                    cx,
                    move |cx| {
                        div()
                            .w_full()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .child(setting_target(
                                SETTINGS_FILE,
                                crate::components::settings::widgets::settings_path_row(
                                    SETTINGS_FILE.id,
                                    SETTINGS_FILE.label,
                                    &path_label,
                                    div()
                                        .flex()
                                        .flex_wrap()
                                        .gap_2()
                                        .child(
                                            quiet_action_button(
                                                "configuration-copy-file-path",
                                                "Copy",
                                                false,
                                                cx,
                                                move |cx| {
                                                    cx.write_to_clipboard(
                                                        ClipboardItem::new_string(
                                                            path_for_copy.clone(),
                                                        ),
                                                    );
                                                    cx.notify();
                                                },
                                            )
                                            .aria_label("Copy settings file path"),
                                        )
                                        .child(
                                            quiet_action_button(
                                                "configuration-open-file",
                                                "Open",
                                                false,
                                                cx,
                                                move |cx| {
                                                    open_configuration_path(
                                                        path_for_open.clone(),
                                                        "Could not open settings file",
                                                    );
                                                    cx.notify();
                                                },
                                            )
                                            .aria_label("Open settings file"),
                                        ),
                                    stacked,
                                ),
                            ))
                            .child(crate::components::settings::widgets::settings_path_row(
                                "advanced-config-folder",
                                "Settings folder",
                                &settings_dir_label,
                                quiet_action_button(
                                    "configuration-open-folder",
                                    "Open",
                                    false,
                                    cx,
                                    move |cx| {
                                        open_configuration_path(
                                            settings_dir.clone(),
                                            "Could not open settings folder",
                                        );
                                        cx.notify();
                                    },
                                )
                                .aria_label("Open settings folder"),
                                stacked,
                            ))
                            .child(crate::components::settings::widgets::settings_path_row(
                                "advanced-backups-folder",
                                "Backups folder",
                                &backups_dir_label,
                                quiet_action_button(
                                    "configuration-open-backups",
                                    "Open",
                                    false,
                                    cx,
                                    move |cx| {
                                        if let Err(error) = std::fs::create_dir_all(&backups_dir) {
                                            backend::push_ui_action(
                                                backend::UiAction::ShowWarningToast {
                                                    message:
                                                        "Could not create settings backups folder"
                                                            .to_string(),
                                                    detail: Some(error.to_string()),
                                                },
                                            );
                                        } else {
                                            open_configuration_path(
                                                backups_dir.clone(),
                                                "Could not open settings backups folder",
                                            );
                                        }
                                        cx.notify();
                                    },
                                )
                                .aria_label("Open settings backups folder"),
                                stacked,
                            ))
                    },
                )),
        ))
        .child(settings_group(
            "advanced-import-settings",
            "Import settings JSON",
            false,
            setting_target(
                SETTINGS_IMPORT,
                settings_summary_disclosure(
                    "advanced-settings-import",
                    SETTINGS_IMPORT.label,
                    &summary,
                    &import_disclosure,
                    cx,
                    move |cx| {
                        div().w_full().min_w_0().child(render_settings_import_panel(
                            editor,
                            escape_focus,
                            window,
                            cx,
                        ))
                    },
                ),
            ),
        ))
        .child(chatterino_import)
}

fn set_settings_import_status(status: String, error: bool) {
    SETTINGS_IMPORT_EDITOR.sync_and_snapshot(recovered_settings_import_editor, |editor| {
        editor.status = Some(status);
        editor.status_is_error = error;
        false
    });
}

fn open_configuration_path(path: std::path::PathBuf, message: &'static str) {
    crate::utils::open_path(path, move |result| {
        if let Err(error) = result {
            backend::push_ui_action(backend::UiAction::ShowWarningToast {
                message: message.to_string(),
                detail: Some(error.to_string()),
            });
        }
    });
}

fn settings_import_summary(editor: &EditorSnapshot<SettingsImportEditor>) -> String {
    if editor.needs_review {
        "Needs review".to_string()
    } else if !editor.draft.json.trim().is_empty() {
        "Unsaved draft".to_string()
    } else if editor.draft.status_is_error {
        "Import/export failed; open for details".to_string()
    } else {
        editor
            .draft
            .status
            .clone()
            .unwrap_or_else(|| "Restore an Echo settings snapshot".to_string())
    }
}

fn render_settings_import_panel<T: 'static>(
    editor: EditorSnapshot<SettingsImportEditor>,
    escape_focus: crate::components::settings::widgets::SettingsDisclosureState,
    window: &mut Window,
    cx: &mut Context<T>,
) -> impl IntoElement + use<T> {
    let editor_generation = editor.generation;
    let editor_needs_review = editor.needs_review;
    let editor = editor.draft;
    let json_input = DraftInput::new(
        SETTINGS_IMPORT.control_id,
        &editor.json,
        "Paste an Echo settings JSON snapshot",
    )
    .label("Settings JSON snapshot")
    .monospace(true)
    .on_focus(|_cx| set_settings_import_focused(true))
    .on_blur(|_cx| set_settings_import_focused(false))
    .on_change(|text, _cx| {
        update_settings_import_editor(|editor| {
            editor.json = text;
            editor.status = None;
            editor.status_is_error = false;
        });
    })
    .on_escape(move |window, cx| {
        set_settings_import_focused(false);
        escape_focus.focus_trigger(window, cx);
    })
    .prepare(window, cx);
    let status_is_error = editor.status_is_error || editor_needs_review;

    div()
        .flex()
        .flex_col()
        .gap_2()
        .child(
            div()
                .text_sm()
                .font_weight(FontWeight::MEDIUM)
                .text_color(colors::text_primary())
                .child("Settings snapshot"),
        )
        .child(json_input.render(cx))
        .child(
            div()
                .flex()
                .flex_wrap()
                .items_center()
                .gap_2()
                .child(quiet_action_button(
                    "configuration-import-json",
                    "Import",
                    false,
                    cx,
                    move |cx| {
                        if let Some(editor) = prepare_settings_import(editor_generation) {
                            match crate::backend::settings::import_settings_from_json(&editor.json)
                            {
                                Ok(result) => {
                                    super::reset_migrated_editor_drafts_after_settings_replace();
                                    let mut message = result
                                        .backup_path
                                        .map(|path| {
                                            format!(
                                                "Imported settings; previous file backed up to {}",
                                                path.display()
                                            )
                                        })
                                        .unwrap_or_else(|| "Imported settings".to_string());
                                    if !result.skipped_launch_settings.is_empty() {
                                        message.push_str(&format!(
                                        ". For safety these sensitive settings were not imported \
                                         and keep their current values: {}",
                                        result.skipped_launch_settings.join(", ")
                                    ));
                                    }
                                    SETTINGS_IMPORT_EDITOR.replace_from_authority(
                                        SettingsImportEditor {
                                            status: Some(message),
                                            ..SettingsImportEditor::default()
                                        },
                                    );
                                }
                                Err(error) => {
                                    update_settings_import_editor(|editor| {
                                        editor.status = Some(error);
                                        editor.status_is_error = true;
                                    });
                                }
                            }
                        }
                        cx.notify();
                    },
                ))
                .child(quiet_action_button(
                    "configuration-clear-json",
                    "Clear",
                    false,
                    cx,
                    |cx| {
                        SETTINGS_IMPORT_EDITOR
                            .replace_from_authority(SettingsImportEditor::default());
                        cx.notify();
                    },
                ))
                .when_some(editor.status, |row, status| {
                    row.child(
                        div()
                            .text_xs()
                            .text_color(if status_is_error {
                                colors::signal_red()
                            } else {
                                colors::text_muted()
                            })
                            .child(status),
                    )
                }),
        )
}

fn set_settings_import_focused(focused: bool) {
    SETTINGS_IMPORT_EDITOR.sync_and_snapshot(recovered_settings_import_editor, |editor| {
        editor.focused = focused;
        false
    });
}

fn recovered_settings_import_editor(_previous: &SettingsImportEditor) -> SettingsImportEditor {
    SettingsImportEditor {
        status: Some(
            "Settings import draft was reset after an internal editor error. Paste or export JSON again before importing."
                .to_string(),
        ),
        status_is_error: true,
        ..SettingsImportEditor::default()
    }
}

fn settings_import_editor_snapshot() -> EditorSnapshot<SettingsImportEditor> {
    SETTINGS_IMPORT_EDITOR.snapshot_or_reset(recovered_settings_import_editor)
}

fn prepare_settings_import(expected_generation: u64) -> Option<SettingsImportEditor> {
    SETTINGS_IMPORT_EDITOR.prepare_ready(expected_generation, recovered_settings_import_editor)
}

fn update_settings_import_editor(update: impl FnOnce(&mut SettingsImportEditor)) -> bool {
    matches!(
        SETTINGS_IMPORT_EDITOR.apply_user_edit(recovered_settings_import_editor, update),
        crate::components::settings::editor_state::EditorEditResult::Applied
    )
}

pub(super) fn reset_settings_import_editor_after_settings_replace() {
    SETTINGS_IMPORT_EDITOR.replace_from_authority(SettingsImportEditor {
        status: Some("Settings import draft reset after settings were replaced".to_string()),
        ..SettingsImportEditor::default()
    });
}

#[cfg(test)]
mod settings_import_editor_tests {
    use super::*;
    use core::prelude::v1::test;

    #[test]
    fn import_summary_distinguishes_recovery_drafts_and_failure() {
        let mut snapshot = EditorSnapshot {
            draft: SettingsImportEditor::default(),
            generation: 0,
            needs_review: false,
        };
        assert_eq!(
            settings_import_summary(&snapshot),
            "Restore an Echo settings snapshot"
        );
        snapshot.draft.json = "{ partial".to_string();
        assert_eq!(settings_import_summary(&snapshot), "Unsaved draft");
        snapshot.needs_review = true;
        assert_eq!(settings_import_summary(&snapshot), "Needs review");
        snapshot.needs_review = false;
        snapshot.draft.json.clear();
        snapshot.draft.status_is_error = true;
        snapshot.draft.status = Some("private error details".to_string());
        assert_eq!(
            settings_import_summary(&snapshot),
            "Import/export failed; open for details"
        );
    }

    #[test]
    fn recovery_discards_unsaved_json_and_focus() {
        let previous = SettingsImportEditor {
            json: "{ partial".to_string(),
            status: Some("old".to_string()),
            status_is_error: false,
            focused: true,
        };

        let recovered = recovered_settings_import_editor(&previous);
        assert!(recovered.json.is_empty());
        assert!(!recovered.focused);
        assert!(recovered.status_is_error);
        assert!(recovered.status.is_some());
    }

    #[test]
    fn general_editor_baseline_restores_every_authoritative_value() {
        let editor = general_settings_editor_from_values(
            "browser".to_string(),
            "player".to_string(),
            "streamlink".to_string(),
            "720p".to_string(),
            "--player mpv".to_string(),
        );

        assert_eq!(editor.browser_path, "browser");
        assert_eq!(editor.custom_player_path, "player");
        assert_eq!(editor.streamlink_path, "streamlink");
        assert_eq!(editor.streamlink_custom_quality, "720p");
        assert_eq!(editor.streamlink_extra_args, "--player mpv");
        assert!(editor.focused.is_none());
    }
}

fn general_settings_editor_from_values(
    browser_path: String,
    custom_player_path: String,
    streamlink_path: String,
    streamlink_custom_quality: String,
    streamlink_extra_args: String,
) -> GeneralSettingsEditor {
    GeneralSettingsEditor {
        browser_path_mode: !browser_path.is_empty(),
        browser_path,
        custom_player_path,
        streamlink_path,
        streamlink_custom_quality,
        streamlink_extra_args,
        ..GeneralSettingsEditor::default()
    }
}

fn recovered_general_settings_editor(_previous: &GeneralSettingsEditor) -> GeneralSettingsEditor {
    general_settings_editor_from_values(
        crate::backend::settings::get_browser_path().unwrap_or_default(),
        crate::backend::settings::get_custom_player_path().unwrap_or_default(),
        crate::backend::settings::get_streamlink_path().unwrap_or_default(),
        crate::backend::settings::get_streamlink_custom_quality(),
        crate::backend::settings::get_streamlink_extra_args(),
    )
}

fn general_settings_editor_snapshot() -> EditorSnapshot<GeneralSettingsEditor> {
    GENERAL_SETTINGS_EDITOR.snapshot_or_reset(recovered_general_settings_editor)
}

fn synced_general_settings_editor(
    browser_path: &str,
    custom_player_path: &str,
    streamlink_path: &str,
    streamlink_custom_quality: &str,
    streamlink_extra_args: &str,
) -> EditorSnapshot<GeneralSettingsEditor> {
    GENERAL_SETTINGS_EDITOR.sync_and_snapshot(
        |_| {
            general_settings_editor_from_values(
                browser_path.to_string(),
                custom_player_path.to_string(),
                streamlink_path.to_string(),
                streamlink_custom_quality.to_string(),
                streamlink_extra_args.to_string(),
            )
        },
        |editor| {
            let mut changed = false;
            if !editor.dirty[general_field_index(GeneralSettingsField::BrowserPath)]
                && editor.browser_path != browser_path
            {
                editor.browser_path = browser_path.to_string();
                editor.browser_path_mode = !browser_path.is_empty();
                changed = true;
            }
            if !editor.dirty[general_field_index(GeneralSettingsField::CustomPlayerPath)]
                && editor.custom_player_path != custom_player_path
            {
                editor.custom_player_path = custom_player_path.to_string();
                changed = true;
            }
            if !editor.dirty[general_field_index(GeneralSettingsField::StreamlinkPath)]
                && editor.streamlink_path != streamlink_path
            {
                editor.streamlink_path = streamlink_path.to_string();
                changed = true;
            }
            if !editor.dirty[general_field_index(GeneralSettingsField::StreamlinkCustomQuality)]
                && editor.streamlink_custom_quality != streamlink_custom_quality
            {
                editor.streamlink_custom_quality = streamlink_custom_quality.to_string();
                changed = true;
            }
            if !editor.dirty[general_field_index(GeneralSettingsField::StreamlinkExtraArgs)]
                && editor.streamlink_extra_args != streamlink_extra_args
            {
                editor.streamlink_extra_args = streamlink_extra_args.to_string();
                changed = true;
            }
            changed
        },
    )
}

fn prepare_general_settings_editor(expected_generation: u64) -> Option<GeneralSettingsEditor> {
    GENERAL_SETTINGS_EDITOR.prepare_ready(expected_generation, recovered_general_settings_editor)
}

fn update_general_settings_editor(update: impl FnOnce(&mut GeneralSettingsEditor)) -> bool {
    matches!(
        GENERAL_SETTINGS_EDITOR.apply_user_edit(recovered_general_settings_editor, update),
        crate::components::settings::editor_state::EditorEditResult::Applied
    )
}

fn apply_general_authoritative_update(update: impl FnOnce(&mut GeneralSettingsEditor)) {
    GENERAL_SETTINGS_EDITOR.apply_authoritative_update(recovered_general_settings_editor, update);
}

pub(super) fn reset_general_settings_editor_after_settings_replace() {
    let mut editor = recovered_general_settings_editor(&GeneralSettingsEditor::default());
    editor.browser_status =
        Some("Player/path drafts reset after settings were replaced".to_string());
    GENERAL_SETTINGS_EDITOR.replace_from_authority(editor);
}

fn optional_path_value(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn save_browser_path_setting(value: &str) -> Result<Option<String>, String> {
    let value = normalize_general_editor_text(GeneralSettingsField::BrowserPath, value)
        .map_err(str::to_string)?;
    crate::backend::settings::set_browser_path_checked(optional_path_value(&value))
}

fn apply_browser_path_save(expected_generation: u64) {
    let Some(editor) = prepare_general_settings_editor(expected_generation) else {
        return;
    };
    match save_browser_path_setting(&editor.browser_path) {
        Ok(path) => apply_general_authoritative_update(|editor| {
            editor.browser_path = path.unwrap_or_default();
            editor.browser_path_mode = !editor.browser_path.is_empty();
            editor.browser_status = Some(if editor.browser_path.is_empty() {
                "Using automatic private-window browser selection".to_string()
            } else {
                "Saved private-window browser executable".to_string()
            });
            editor.browser_status_is_error = false;
            editor.focused = None;
            editor.dirty[general_field_index(GeneralSettingsField::BrowserPath)] = false;
        }),
        Err(error) => apply_general_authoritative_update(|editor| {
            editor.browser_status = Some(error);
            editor.browser_status_is_error = true;
        }),
    }
}

fn save_custom_player_path_setting(value: &str) -> Result<Option<String>, String> {
    let value = normalize_general_editor_text(GeneralSettingsField::CustomPlayerPath, value)
        .map_err(str::to_string)?;
    crate::backend::settings::set_custom_player_path_checked(optional_path_value(&value))
}

fn apply_custom_player_path_save(expected_generation: u64) {
    let Some(editor) = prepare_general_settings_editor(expected_generation) else {
        return;
    };
    match save_custom_player_path_setting(&editor.custom_player_path) {
        Ok(path) => apply_general_authoritative_update(|editor| {
            editor.custom_player_path = path.unwrap_or_default();
            editor.custom_player_status = Some(if editor.custom_player_path.is_empty() {
                "No custom player executable configured".to_string()
            } else {
                "Saved custom player executable".to_string()
            });
            editor.custom_player_status_is_error = false;
            editor.focused = None;
            editor.dirty[general_field_index(GeneralSettingsField::CustomPlayerPath)] = false;
        }),
        Err(error) => apply_general_authoritative_update(|editor| {
            editor.custom_player_status = Some(error);
            editor.custom_player_status_is_error = true;
        }),
    }
}

fn save_streamlink_path_setting(value: &str) -> Result<Option<String>, String> {
    let value = normalize_general_editor_text(GeneralSettingsField::StreamlinkPath, value)
        .map_err(str::to_string)?;
    crate::backend::settings::set_streamlink_path_checked(optional_path_value(&value))
}

fn apply_streamlink_path_save(expected_generation: u64) {
    let Some(editor) = prepare_general_settings_editor(expected_generation) else {
        return;
    };
    match save_streamlink_path_setting(&editor.streamlink_path) {
        Ok(path) => apply_general_authoritative_update(|editor| {
            editor.streamlink_path = path.unwrap_or_default();
            editor.streamlink_status = Some(if editor.streamlink_path.is_empty() {
                "Using streamlink from PATH".to_string()
            } else {
                "Saved Streamlink executable".to_string()
            });
            editor.streamlink_status_is_error = false;
            editor.focused = None;
            editor.dirty[general_field_index(GeneralSettingsField::StreamlinkPath)] = false;
        }),
        Err(error) => apply_general_authoritative_update(|editor| {
            editor.streamlink_status = Some(error);
            editor.streamlink_status_is_error = true;
        }),
    }
}

fn save_streamlink_custom_quality_setting(value: &str) -> String {
    let quality = single_line_text(value).trim().to_string();
    crate::backend::settings::set_streamlink_custom_quality(quality.clone());
    quality
}

fn apply_streamlink_custom_quality_save(expected_generation: u64) {
    let Some(editor) = prepare_general_settings_editor(expected_generation) else {
        return;
    };
    let quality = save_streamlink_custom_quality_setting(&editor.streamlink_custom_quality);
    apply_general_authoritative_update(|editor| {
        editor.streamlink_custom_quality = quality;
        editor.streamlink_custom_quality_status =
            Some(if editor.streamlink_custom_quality.is_empty() {
                "Custom quality will fall back to best".to_string()
            } else {
                "Saved custom Streamlink quality".to_string()
            });
        editor.streamlink_custom_quality_status_is_error = false;
        editor.focused = None;
        editor.dirty[general_field_index(GeneralSettingsField::StreamlinkCustomQuality)] = false;
    });
}

fn save_streamlink_extra_args_setting(value: &str) -> Result<String, String> {
    let args = normalize_general_editor_text(GeneralSettingsField::StreamlinkExtraArgs, value)
        .map_err(str::to_string)?;
    crate::backend::commands::validate_streamlink_extra_args(&args)?;
    crate::backend::settings::set_streamlink_extra_args(args.clone());
    Ok(args)
}

fn apply_streamlink_extra_args_save(expected_generation: u64) {
    let Some(editor) = prepare_general_settings_editor(expected_generation) else {
        return;
    };
    match save_streamlink_extra_args_setting(&editor.streamlink_extra_args) {
        Ok(args) => apply_general_authoritative_update(|editor| {
            editor.streamlink_extra_args = args;
            editor.streamlink_extra_args_status =
                Some(if editor.streamlink_extra_args.is_empty() {
                    "No Streamlink extra arguments".to_string()
                } else {
                    "Saved Streamlink extra arguments".to_string()
                });
            editor.streamlink_extra_args_status_is_error = false;
            editor.focused = None;
            editor.dirty[general_field_index(GeneralSettingsField::StreamlinkExtraArgs)] = false;
        }),
        Err(error) => apply_general_authoritative_update(|editor| {
            editor.streamlink_extra_args_status = Some(error);
            editor.streamlink_extra_args_status_is_error = true;
        }),
    }
}

fn general_editor_field_parts(
    editor: &mut GeneralSettingsEditor,
    field: GeneralSettingsField,
) -> (&mut String, &mut Option<String>, &mut bool) {
    match field {
        GeneralSettingsField::BrowserPath => (
            &mut editor.browser_path,
            &mut editor.browser_status,
            &mut editor.browser_status_is_error,
        ),
        GeneralSettingsField::CustomPlayerPath => (
            &mut editor.custom_player_path,
            &mut editor.custom_player_status,
            &mut editor.custom_player_status_is_error,
        ),
        GeneralSettingsField::StreamlinkPath => (
            &mut editor.streamlink_path,
            &mut editor.streamlink_status,
            &mut editor.streamlink_status_is_error,
        ),
        GeneralSettingsField::StreamlinkCustomQuality => (
            &mut editor.streamlink_custom_quality,
            &mut editor.streamlink_custom_quality_status,
            &mut editor.streamlink_custom_quality_status_is_error,
        ),
        GeneralSettingsField::StreamlinkExtraArgs => (
            &mut editor.streamlink_extra_args,
            &mut editor.streamlink_extra_args_status,
            &mut editor.streamlink_extra_args_status_is_error,
        ),
    }
}

fn save_general_settings_field(generation: u64, field: GeneralSettingsField) {
    match field {
        GeneralSettingsField::BrowserPath => apply_browser_path_save(generation),
        GeneralSettingsField::CustomPlayerPath => apply_custom_player_path_save(generation),
        GeneralSettingsField::StreamlinkPath => apply_streamlink_path_save(generation),
        GeneralSettingsField::StreamlinkCustomQuality => {
            apply_streamlink_custom_quality_save(generation);
        }
        GeneralSettingsField::StreamlinkExtraArgs => apply_streamlink_extra_args_save(generation),
    }
}
