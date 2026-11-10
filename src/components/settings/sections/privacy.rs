//! Privacy settings section - Streamer mode and privacy controls
//!
//! Streamer mode with:
//! - Auto-detection of OBS/Streamlabs/XSplit
//! - Manual override (Auto/Always On/Disabled)
//! - Granular privacy controls (hide avatars, links, viewer count, etc.)

use crate::backend::streamer_mode::{
    self, StreamerModeStatus, get_cached_detection_status, get_streamer_mode_settings,
    is_streamer_mode_active_cached, refresh_detection, set_streamer_mode_status,
    update_streamer_mode_settings,
};
use crate::components::settings::settings_navigation::{
    SettingDescriptor, SettingsSectionId, SettingsSubsectionId,
};
use crate::components::settings::settings_page::SettingsPage;
use crate::components::settings::widgets::{
    SelectOption, SettingsPageUi, button, labelled_toggle, setting_reset, setting_row,
    setting_target, settings_disclosure, settings_group, settings_group_contents, settings_select,
};
use crate::theme::colors;
use gpui::prelude::FluentBuilder;
use gpui::*;

const STREAMER_MODE_STATUS: SettingDescriptor = SettingDescriptor {
    id: "streamer-mode.status",
    label: "Streamer mode",
    aliases: &["auto", "always on", "disabled", "obs", "detection"],
    control_id: "streamer-mode",
    disclosures: &[],
};

const HIDE_AVATARS: SettingDescriptor = SettingDescriptor {
    id: "streamer-mode.hide-avatars",
    label: "Hide avatars",
    aliases: &["profile picture", "user card"],
    control_id: "hide-avatars",
    disclosures: &["general-protection-details"],
};

const HIDE_USER_CARD_DETAILS: SettingDescriptor = SettingDescriptor {
    id: "streamer-mode.hide-user-card-details",
    label: "Hide user card details",
    aliases: &["identity", "local history", "recent messages"],
    control_id: "hide-user-card-details",
    disclosures: &["general-protection-details"],
};

const HIDE_ACCOUNT_NAMES: SettingDescriptor = SettingDescriptor {
    id: "streamer-mode.hide-account-names",
    label: "Hide account names",
    aliases: &["account switcher", "login"],
    control_id: "hide-account-names",
    disclosures: &["general-protection-details"],
};

const HIDE_LINK_THUMBNAILS: SettingDescriptor = SettingDescriptor {
    id: "streamer-mode.hide-link-thumbnails",
    label: "Hide link thumbnails",
    aliases: &["preview image", "embed"],
    control_id: "hide-thumbnails",
    disclosures: &["general-protection-details"],
};

const HIDE_LINK_TEXT: SettingDescriptor = SettingDescriptor {
    id: "streamer-mode.hide-link-text",
    label: "Hide link text",
    aliases: &["url", "placeholder"],
    control_id: "hide-link-urls",
    disclosures: &["general-protection-details"],
};

const HIDE_VIEWER_COUNT: SettingDescriptor = SettingDescriptor {
    id: "streamer-mode.hide-viewer-count",
    label: "Hide viewer count",
    aliases: &["viewers", "uptime", "stream duration"],
    control_id: "hide-viewers",
    disclosures: &["general-protection-details"],
};

const HIDE_MOD_ACTIONS: SettingDescriptor = SettingDescriptor {
    id: "streamer-mode.hide-mod-actions",
    label: "Hide mod action details",
    aliases: &["moderation", "timeout", "ban"],
    control_id: "hide-mod-actions",
    disclosures: &["general-protection-details"],
};

const HIDE_AUTOMOD_TERMS: SettingDescriptor = SettingDescriptor {
    id: "streamer-mode.hide-automod-terms",
    label: "Hide AutoMod terms",
    aliases: &["automod", "held message", "flagged"],
    control_id: "hide-automod-terms",
    disclosures: &["general-protection-details"],
};

const HIDE_WHISPER_CONTENT: SettingDescriptor = SettingDescriptor {
    id: "streamer-mode.hide-whisper-content",
    label: "Hide whisper content",
    aliases: &["whisper", "dm", "placeholder"],
    control_id: "hide-whispers",
    disclosures: &["general-protection-details"],
};

const HIDE_MENTION_CONTENT: SettingDescriptor = SettingDescriptor {
    id: "streamer-mode.hide-mention-content",
    label: "Hide mention content",
    aliases: &["mention", "activity", "placeholder"],
    control_id: "hide-mentions",
    disclosures: &["general-protection-details"],
};

/// Streamer Mode › Privacy.
pub const STREAMER_MODE_PRIVACY_SETTINGS: &[SettingDescriptor] = &[
    STREAMER_MODE_STATUS,
    HIDE_AVATARS,
    HIDE_USER_CARD_DETAILS,
    HIDE_ACCOUNT_NAMES,
    HIDE_LINK_THUMBNAILS,
    HIDE_LINK_TEXT,
    HIDE_VIEWER_COUNT,
    HIDE_MOD_ACTIONS,
    HIDE_AUTOMOD_TERMS,
    HIDE_WHISPER_CONTENT,
    HIDE_MENTION_CONTENT,
];

const MUTE_MENTION_SOUNDS: SettingDescriptor = SettingDescriptor {
    id: "streamer-mode.mute-mention-sounds",
    label: "Mute mention sounds while active",
    aliases: &["silence", "alert", "ping"],
    control_id: "mute-mentions",
    disclosures: &["general-protection-details"],
};

const SUPPRESS_DESKTOP_NOTIFICATIONS: SettingDescriptor = SettingDescriptor {
    id: "streamer-mode.suppress-notifications",
    label: "Suppress desktop notifications while active",
    aliases: &["toast", "system notification"],
    control_id: "suppress-notif",
    disclosures: &["general-protection-details"],
};

/// Streamer Mode › Notifications.
pub const STREAMER_MODE_NOTIFICATIONS_SETTINGS: &[SettingDescriptor] =
    &[MUTE_MENTION_SOUNDS, SUPPRESS_DESKTOP_NOTIFICATIONS];

pub const STREAMER_MODE_GENERAL_SETTINGS: &[SettingDescriptor] = &[
    STREAMER_MODE_STATUS,
    HIDE_AVATARS,
    HIDE_USER_CARD_DETAILS,
    HIDE_ACCOUNT_NAMES,
    HIDE_LINK_THUMBNAILS,
    HIDE_LINK_TEXT,
    HIDE_VIEWER_COUNT,
    HIDE_MOD_ACTIONS,
    HIDE_AUTOMOD_TERMS,
    HIDE_WHISPER_CONTENT,
    HIDE_MENTION_CONTENT,
    MUTE_MENTION_SOUNDS,
    SUPPRESS_DESKTOP_NOTIFICATIONS,
];

const CLEAR_MESSAGE_CACHE: SettingDescriptor = SettingDescriptor {
    id: "streamer-mode.clear-message-cache",
    label: "Clear message cache",
    aliases: &["history", "storage", "purge"],
    control_id: "clear-message-cache",
    disclosures: &[],
};

const RESET_ALL_SETTINGS: SettingDescriptor = SettingDescriptor {
    id: "streamer-mode.reset-all-settings",
    label: "Reset all settings",
    aliases: &["factory reset", "defaults"],
    control_id: "reset-all-settings",
    disclosures: &[],
};

/// Streamer Mode › Data management.
pub const STREAMER_MODE_DATA_SETTINGS: &[SettingDescriptor] =
    &[CLEAR_MESSAGE_CACHE, RESET_ALL_SETTINGS];

/// Render Streamer Mode as one calm primary row and an expandable protection list.
pub fn render_privacy_section<T: 'static>(
    ui: &mut SettingsPageUi,
    cx: &mut Context<T>,
) -> SettingsPage {
    let settings = get_streamer_mode_settings();
    let defaults = streamer_mode::StreamerModeSettings::default();
    let is_active = is_streamer_mode_active_cached();
    let (is_detected, detection_message) = get_cached_detection_status();
    let protection_state = ui.disclosure("general-protection-details", cx);
    let protection_modified = protection_modified_count(&settings, &defaults);
    let mode_select = ui.select("streamer-mode-select", cx);
    let stacked = ui.stacked;

    SettingsPage::new(SettingsSectionId::Privacy).subsection(
        SettingsSubsectionId::StreamerPrivacy,
        settings_group(
            "general-streamer-mode",
            "Streamer mode",
            false,
            div()
                .w_full()
                .min_w_0()
                .flex()
                .flex_col()
                .gap_3()
                .child(setting_target(
                    STREAMER_MODE_STATUS,
                    setting_row(
                        STREAMER_MODE_STATUS.id,
                        STREAMER_MODE_STATUS.label,
                        Some("Choose when Echo protects details on screen."),
                        render_streamer_mode_control(
                            settings.enabled,
                            is_active,
                            is_detected,
                            detection_message,
                            &mode_select,
                            cx,
                        ),
                        settings.enabled != defaults.enabled,
                        (settings.enabled != defaults.enabled).then(|| {
                            setting_reset(
                                "reset-streamer-mode",
                                STREAMER_MODE_STATUS.label,
                                cx,
                                |cx| {
                                    set_streamer_mode_status(StreamerModeStatus::default());
                                    cx.notify();
                                },
                            )
                        }),
                        stacked,
                    ),
                ))
                .child(settings_disclosure(
                    "general-protection-details",
                    "Protection details",
                    Some("12 options"),
                    protection_modified,
                    &protection_state,
                    cx,
                    move |cx| {
                        settings_group_contents(
                            "general-protection-details-content",
                            "Protection details",
                            false,
                            render_protection_options(&settings, stacked, cx),
                        )
                    },
                )),
        ),
    )
}

fn render_streamer_mode_control<T: 'static>(
    current: StreamerModeStatus,
    is_active: bool,
    is_detected: bool,
    detection_message: Option<String>,
    select: &crate::components::settings::widgets::SettingsSelectState,
    cx: &mut Context<T>,
) -> impl IntoElement + use<T> {
    let selected = streamer_mode_status_value(current);
    let options = [
        SelectOption::new("auto", "Auto"),
        SelectOption::new("always-on", "Always on"),
        SelectOption::new("disabled", "Disabled"),
    ];
    let status = if is_active { "Active" } else { "Inactive" };
    let detail = detection_message.unwrap_or_default();

    div()
        .w_full()
        .min_w_0()
        .flex()
        .flex_col()
        .gap_2()
        .child(settings_select(
            "streamer-mode-select",
            STREAMER_MODE_STATUS.label,
            &options,
            selected,
            select,
            cx,
            |value, cx| {
                let status = match value.as_str() {
                    "always-on" => StreamerModeStatus::AlwaysOn,
                    "disabled" => StreamerModeStatus::Disabled,
                    _ => StreamerModeStatus::Auto,
                };
                set_streamer_mode_status(status);
                cx.notify();
            },
        ))
        .child(
            div()
                .flex()
                .flex_wrap()
                .items_center()
                .gap_2()
                .child(div().w(px(7.0)).h(px(7.0)).rounded_full().bg(if is_active {
                    colors::success()
                } else {
                    colors::text_muted()
                }))
                .child(
                    div()
                        .text_xs()
                        .text_color(if is_active {
                            colors::success()
                        } else {
                            colors::text_secondary()
                        })
                        .child(status),
                )
                .when(is_detected && current == StreamerModeStatus::Auto, |row| {
                    row.child(
                        div()
                            .min_w_0()
                            .text_xs()
                            .text_color(colors::text_muted())
                            .text_ellipsis()
                            .child(detail),
                    )
                })
                .when(current == StreamerModeStatus::Auto, |row| {
                    row.child(button("Refresh", false, |_cx| refresh_detection(), cx))
                }),
        )
}

#[derive(Clone, Copy)]
struct ProtectionToggle {
    descriptor: SettingDescriptor,
    help: &'static str,
    get: fn(&streamer_mode::StreamerModeSettings) -> bool,
    set: fn(&mut streamer_mode::StreamerModeSettings, bool),
}

const PROTECTION_TOGGLES: &[ProtectionToggle] = &[
    ProtectionToggle {
        descriptor: HIDE_AVATARS,
        help: "Hide user avatars in user cards",
        get: |settings| settings.hide_avatars,
        set: |settings, value| settings.hide_avatars = value,
    },
    ProtectionToggle {
        descriptor: HIDE_USER_CARD_DETAILS,
        help: "Hide user-card identities, local history, and recent messages",
        get: |settings| settings.hide_user_card_details,
        set: |settings, value| settings.hide_user_card_details = value,
    },
    ProtectionToggle {
        descriptor: HIDE_ACCOUNT_NAMES,
        help: "Hide local account names in account switchers",
        get: |settings| settings.hide_account_names,
        set: |settings, value| settings.hide_account_names = value,
    },
    ProtectionToggle {
        descriptor: HIDE_LINK_THUMBNAILS,
        help: "Hide preview images for links",
        get: |settings| settings.hide_link_thumbnails,
        set: |settings, value| settings.hide_link_thumbnails = value,
    },
    ProtectionToggle {
        descriptor: HIDE_LINK_TEXT,
        help: "Replace visible chat URLs with placeholders",
        get: |settings| settings.hide_link_urls,
        set: |settings, value| settings.hide_link_urls = value,
    },
    ProtectionToggle {
        descriptor: HIDE_VIEWER_COUNT,
        help: "Hide viewer count and stream duration",
        get: |settings| settings.hide_viewer_count,
        set: |settings, value| settings.hide_viewer_count = value,
    },
    ProtectionToggle {
        descriptor: HIDE_MOD_ACTIONS,
        help: "Hide specific moderation action details",
        get: |settings| settings.hide_mod_actions,
        set: |settings, value| settings.hide_mod_actions = value,
    },
    ProtectionToggle {
        descriptor: HIDE_AUTOMOD_TERMS,
        help: "Hide flagged AutoMod terms and held-message details",
        get: |settings| settings.hide_automod_terms,
        set: |settings, value| settings.hide_automod_terms = value,
    },
    ProtectionToggle {
        descriptor: HIDE_WHISPER_CONTENT,
        help: "Show placeholders instead of whisper text",
        get: |settings| settings.hide_whisper_content,
        set: |settings, value| settings.hide_whisper_content = value,
    },
    ProtectionToggle {
        descriptor: HIDE_MENTION_CONTENT,
        help: "Show placeholders in Activity Mentions",
        get: |settings| settings.hide_mention_content,
        set: |settings, value| settings.hide_mention_content = value,
    },
    ProtectionToggle {
        descriptor: MUTE_MENTION_SOUNDS,
        help: "Silence mention alert sounds while Streamer Mode is active",
        get: |settings| settings.mute_mentions,
        set: |settings, value| settings.mute_mentions = value,
    },
    ProtectionToggle {
        descriptor: SUPPRESS_DESKTOP_NOTIFICATIONS,
        help: "Prevent desktop notifications while Streamer Mode is active",
        get: |settings| settings.suppress_notifications,
        set: |settings, value| settings.suppress_notifications = value,
    },
];

fn render_protection_options<T: 'static>(
    settings: &streamer_mode::StreamerModeSettings,
    stacked: bool,
    cx: &mut Context<T>,
) -> impl IntoElement + use<T> {
    let rows: Vec<AnyElement> = PROTECTION_TOGGLES
        .iter()
        .copied()
        .map(|option| render_protection_toggle(option, (option.get)(settings), stacked, cx))
        .collect();

    div().w_full().min_w_0().flex().flex_col().children(rows)
}

fn render_protection_toggle<T: 'static>(
    option: ProtectionToggle,
    value: bool,
    stacked: bool,
    cx: &mut Context<T>,
) -> AnyElement {
    let default = streamer_mode::StreamerModeSettings::default();
    let default_value = (option.get)(&default);
    let modified = value != default_value;
    let set = option.set;
    let reset = modified.then(|| {
        setting_reset(
            format!("reset-{}", option.descriptor.control_id),
            option.descriptor.label,
            cx,
            move |cx| {
                update_streamer_mode_settings(|settings| set(settings, default_value));
                cx.notify();
            },
        )
    });
    let set = option.set;

    setting_target(
        option.descriptor,
        crate::components::settings::widgets::setting_compact_row(
            option.descriptor.id,
            option.descriptor.label,
            Some(option.help),
            labelled_toggle(
                option.descriptor.control_id,
                option.descriptor.label,
                value,
                cx,
                move |next, cx| {
                    update_streamer_mode_settings(|settings| set(settings, next));
                    cx.notify();
                },
            ),
            modified,
            reset,
            stacked,
        ),
    )
    .into_any_element()
}

fn protection_modified_count(
    settings: &streamer_mode::StreamerModeSettings,
    defaults: &streamer_mode::StreamerModeSettings,
) -> usize {
    PROTECTION_TOGGLES
        .iter()
        .filter(|option| (option.get)(settings) != (option.get)(defaults))
        .count()
}

fn streamer_mode_status_value(status: StreamerModeStatus) -> &'static str {
    match status {
        StreamerModeStatus::Auto => "auto",
        StreamerModeStatus::AlwaysOn => "always-on",
        StreamerModeStatus::Disabled => "disabled",
    }
}

/// Delete the persisted message history cache and report the outcome as a
/// toast. Runs once the user confirms `UiAction::ConfirmClearMessageCache`.
pub(crate) fn clear_message_cache() {
    match crate::backend::history_cache::clear_history_cache() {
        Ok(removed) => {
            crate::log_ui!(info, removed, "Cleared message history cache");
            crate::backend::push_ui_action(crate::backend::UiAction::ShowSuccessToast {
                message: "Message cache cleared".to_string(),
                detail: Some(format!("Removed {removed} persisted history cache file(s)")),
            });
        }
        Err(error) => {
            crate::backend::push_ui_action(crate::backend::UiAction::ShowWarningToast {
                message: "Couldn't clear the message cache".to_string(),
                detail: Some(error),
            });
        }
    }
}
