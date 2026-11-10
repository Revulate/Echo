//! Sounds settings section with interactive controls wired to backend

use crate::backend;
use crate::backend::sound_events::supports_desktop_notification;
use crate::components::settings::settings_navigation::{
    SettingDescriptor, SettingsSectionId, SettingsSubsectionId,
};
use crate::components::settings::settings_page::SettingsPage;
use crate::components::settings::widgets::{
    SettingsMetrics, SettingsPageUi, SliderDrag, labelled_toggle, setting_float_modified,
    setting_reset, setting_row_with_enabled, setting_target, settings_defaults,
    settings_disclosure_with_layout, settings_group, settings_group_contents, settings_subheading,
    settings_summary_disclosure, toggle_with_enabled,
};
use crate::components::ui::ButtonExt;
use crate::theme::colors;
use crate::theme::tokens::{Radii, RadiiPixels, Spacing};
use gpui::prelude::FluentBuilder;
use gpui::*;
use std::cell::Cell;
use std::rc::Rc;
use std::sync::LazyLock;

const SOUND_EVENTS: SettingDescriptor = SettingDescriptor {
    id: "notifications.sound-events",
    label: "Sound events",
    aliases: &["audio", "alert", "ping", "custom sound", "volume"],
    control_id: "sound-events",
    disclosures: &[SOUND_EVENTS_DISCLOSURE],
};

const SOUND_MASTER_ENABLED: SettingDescriptor = SettingDescriptor {
    id: "notifications.sound-master-enabled",
    label: "Enable all sounds",
    aliases: &["master sound", "mute all", "audio enabled"],
    control_id: "toggle-global-sound",
    disclosures: &[],
};

const SOUND_MASTER_VOLUME: SettingDescriptor = SettingDescriptor {
    id: "notifications.sound-master-volume",
    label: "Master sound volume",
    aliases: &["master volume", "audio volume", "all sounds"],
    control_id: "vol-bar-master",
    disclosures: &[],
};

#[derive(Clone, Copy)]
enum SoundEventField {
    Enabled,
    Volume,
    Preview,
}

macro_rules! sound_event_descriptors {
    ($($enabled:ident, $volume:ident, $preview:ident, $variant:ident, $slug:literal, $label:literal, $legacy_label:literal);+ $(;)?) => {
        $(
            const $enabled: SettingDescriptor = SettingDescriptor {
                id: concat!("notifications.sound-event.", $slug, ".enabled"),
                label: concat!($label, " sound enabled"),
                aliases: &[$label, $legacy_label, $slug],
                control_id: concat!("sound-event-", $slug, "-enabled"),
                disclosures: &[SOUND_EVENTS_DISCLOSURE],
            };

            const $volume: SettingDescriptor = SettingDescriptor {
                id: concat!("notifications.sound-event.", $slug, ".volume"),
                label: concat!($label, " sound volume"),
                aliases: &[$label, $legacy_label, $slug],
                control_id: concat!("sound-event-", $slug, "-volume"),
                disclosures: &[SOUND_EVENTS_DISCLOSURE],
            };

            const $preview: SettingDescriptor = SettingDescriptor {
                id: concat!("notifications.sound-event.", $slug, ".preview"),
                label: concat!("Preview ", $label, " sound"),
                aliases: &[$label, $legacy_label, $slug],
                control_id: concat!("sound-event-", $slug, "-preview"),
                disclosures: &[SOUND_EVENTS_DISCLOSURE],
            };
        )+

        fn sound_event_descriptor(
            event_type: &backend::SoundEventType,
            field: SoundEventField,
        ) -> SettingDescriptor {
            match (event_type, field) {
                $(
                    (&backend::SoundEventType::$variant, SoundEventField::Enabled) => $enabled,
                    (&backend::SoundEventType::$variant, SoundEventField::Volume) => $volume,
                    (&backend::SoundEventType::$variant, SoundEventField::Preview) => $preview,
                )+
            }
        }
    };
}

sound_event_descriptors!(
    MENTION_SOUND_ENABLED, MENTION_SOUND_VOLUME, MENTION_SOUND_PREVIEW, Mention, "mention", "Mention", "Mention";
    WHISPER_SOUND_ENABLED, WHISPER_SOUND_VOLUME, WHISPER_SOUND_PREVIEW, Whisper, "whisper", "Whisper", "Whisper";
    HIGHLIGHTED_MESSAGE_SOUND_ENABLED, HIGHLIGHTED_MESSAGE_SOUND_VOLUME, HIGHLIGHTED_MESSAGE_SOUND_PREVIEW, HighlightedMessage, "highlighted-message", "Highlighted message", "Highlighted Message";
    FIRST_MESSAGE_SOUND_ENABLED, FIRST_MESSAGE_SOUND_VOLUME, FIRST_MESSAGE_SOUND_PREVIEW, FirstMessage, "first-message", "First message", "First Message";
    RETURNING_CHATTER_SOUND_ENABLED, RETURNING_CHATTER_SOUND_VOLUME, RETURNING_CHATTER_SOUND_PREVIEW, ReturningChatter, "returning-chatter", "Returning chatter", "Returning Chatter";
    SUBSCRIPTION_SOUND_ENABLED, SUBSCRIPTION_SOUND_VOLUME, SUBSCRIPTION_SOUND_PREVIEW, Subscription, "subscription", "Subscription", "Subscription";
    RAID_SOUND_ENABLED, RAID_SOUND_VOLUME, RAID_SOUND_PREVIEW, Raid, "raid", "Raid", "Raid";
    BITS_BADGE_SOUND_ENABLED, BITS_BADGE_SOUND_VOLUME, BITS_BADGE_SOUND_PREVIEW, BitsBadge, "bits-badge", "Bits badge", "Bits Badge";
    ANNOUNCEMENT_SOUND_ENABLED, ANNOUNCEMENT_SOUND_VOLUME, ANNOUNCEMENT_SOUND_PREVIEW, Announcement, "announcement", "Announcement", "Announcement";
    PHRASE_SOUND_ENABLED, PHRASE_SOUND_VOLUME, PHRASE_SOUND_PREVIEW, Phrase, "phrase", "Phrase match", "Phrase Match";
    CHANNEL_POINT_REDEEM_SOUND_ENABLED, CHANNEL_POINT_REDEEM_SOUND_VOLUME, CHANNEL_POINT_REDEEM_SOUND_PREVIEW, ChannelPointRedeem, "channel-point-redeem", "Channel point redeem", "Channel Point Redeem";
    STREAM_ONLINE_SOUND_ENABLED, STREAM_ONLINE_SOUND_VOLUME, STREAM_ONLINE_SOUND_PREVIEW, StreamOnline, "stream-online", "Stream online", "Stream Online";
    STREAM_OFFLINE_SOUND_ENABLED, STREAM_OFFLINE_SOUND_VOLUME, STREAM_OFFLINE_SOUND_PREVIEW, StreamOffline, "stream-offline", "Stream offline", "Stream Offline";
);

/// Notifications › Sounds.
pub const NOTIFICATIONS_SOUNDS_SETTINGS: &[SettingDescriptor] = &[
    SOUND_EVENTS,
    SOUND_MASTER_ENABLED,
    SOUND_MASTER_VOLUME,
    MENTION_SOUND_ENABLED,
    MENTION_SOUND_VOLUME,
    MENTION_SOUND_PREVIEW,
    WHISPER_SOUND_ENABLED,
    WHISPER_SOUND_VOLUME,
    WHISPER_SOUND_PREVIEW,
    HIGHLIGHTED_MESSAGE_SOUND_ENABLED,
    HIGHLIGHTED_MESSAGE_SOUND_VOLUME,
    HIGHLIGHTED_MESSAGE_SOUND_PREVIEW,
    FIRST_MESSAGE_SOUND_ENABLED,
    FIRST_MESSAGE_SOUND_VOLUME,
    FIRST_MESSAGE_SOUND_PREVIEW,
    RETURNING_CHATTER_SOUND_ENABLED,
    RETURNING_CHATTER_SOUND_VOLUME,
    RETURNING_CHATTER_SOUND_PREVIEW,
    SUBSCRIPTION_SOUND_ENABLED,
    SUBSCRIPTION_SOUND_VOLUME,
    SUBSCRIPTION_SOUND_PREVIEW,
    RAID_SOUND_ENABLED,
    RAID_SOUND_VOLUME,
    RAID_SOUND_PREVIEW,
    BITS_BADGE_SOUND_ENABLED,
    BITS_BADGE_SOUND_VOLUME,
    BITS_BADGE_SOUND_PREVIEW,
    ANNOUNCEMENT_SOUND_ENABLED,
    ANNOUNCEMENT_SOUND_VOLUME,
    ANNOUNCEMENT_SOUND_PREVIEW,
    PHRASE_SOUND_ENABLED,
    PHRASE_SOUND_VOLUME,
    PHRASE_SOUND_PREVIEW,
    CHANNEL_POINT_REDEEM_SOUND_ENABLED,
    CHANNEL_POINT_REDEEM_SOUND_VOLUME,
    CHANNEL_POINT_REDEEM_SOUND_PREVIEW,
    STREAM_ONLINE_SOUND_ENABLED,
    STREAM_ONLINE_SOUND_VOLUME,
    STREAM_ONLINE_SOUND_PREVIEW,
    STREAM_OFFLINE_SOUND_ENABLED,
    STREAM_OFFLINE_SOUND_VOLUME,
    STREAM_OFFLINE_SOUND_PREVIEW,
];

const DESKTOP_NOTIFICATIONS: SettingDescriptor = SettingDescriptor {
    id: "notifications.desktop",
    label: "Desktop notifications",
    aliases: &["toast", "system notification", "os notification"],
    control_id: "desktop-notifications",
    disclosures: &[],
};

/// Notifications › Desktop notifications.
pub const NOTIFICATIONS_DESKTOP_SETTINGS: &[SettingDescriptor] = &[DESKTOP_NOTIFICATIONS];

const LIVE_STATUS: SettingDescriptor = SettingDescriptor {
    id: "notifications.live-status",
    label: "Show channel live status",
    aliases: &["stream online", "offline", "activity events"],
    control_id: "live-notifications",
    disclosures: &[],
};

const LIVE_STARTUP_DELAY: SettingDescriptor = SettingDescriptor {
    id: "notifications.live-startup-delay",
    label: "Startup delay",
    aliases: &["suppress", "startup", "grace period"],
    control_id: "live-delay",
    disclosures: &[NOTIFICATIONS_MORE_DISCLOSURE],
};

/// Notifications › Live channels.
pub const NOTIFICATIONS_LIVE_SETTINGS: &[SettingDescriptor] = &[LIVE_STATUS, LIVE_STARTUP_DELAY];

pub const NOTIFICATIONS_PRIMARY_SETTINGS: &[SettingDescriptor] =
    &[DESKTOP_NOTIFICATIONS, LIVE_STATUS, LIVE_STARTUP_DELAY];

const MUTED_CHANNELS: SettingDescriptor = SettingDescriptor {
    id: "notifications.muted-channels",
    label: "Muted channels",
    aliases: &[
        "muted channel highlight sounds",
        "mute",
        "silence",
        "per channel",
    ],
    control_id: "muted-channels",
    disclosures: &[MUTED_CHANNELS_DISCLOSURE],
};

const SOUND_EVENTS_DISCLOSURE: &str = "sounds-events";
const NOTIFICATIONS_MORE_DISCLOSURE: &str = "notifications-more";
const MUTED_CHANNELS_DISCLOSURE: &str = "notifications-muted";

fn sound_defaults() -> &'static backend::sounds::SoundSettings {
    static DEFAULTS: LazyLock<backend::sounds::SoundSettings> =
        LazyLock::new(backend::sounds::SoundSettings::default);
    &DEFAULTS
}

fn sound_event_default(event_type: backend::SoundEventType) -> backend::sounds::SoundEventSettings {
    let event_type: backend::sounds::SoundEventType = event_type.into();
    event_type.default_settings()
}

fn sound_event_display_name(event_type: backend::SoundEventType) -> &'static str {
    match event_type {
        backend::SoundEventType::Mention => "Mention",
        backend::SoundEventType::Whisper => "Whisper",
        backend::SoundEventType::HighlightedMessage => "Highlighted message",
        backend::SoundEventType::FirstMessage => "First message",
        backend::SoundEventType::ReturningChatter => "Returning chatter",
        backend::SoundEventType::Subscription => "Subscription",
        backend::SoundEventType::Raid => "Raid",
        backend::SoundEventType::BitsBadge => "Bits badge",
        backend::SoundEventType::Announcement => "Announcement",
        backend::SoundEventType::Phrase => "Phrase match",
        backend::SoundEventType::ChannelPointRedeem => "Channel point redeem",
        backend::SoundEventType::StreamOnline => "Stream online",
        backend::SoundEventType::StreamOffline => "Stream offline",
    }
}

/// Notifications › Muted channels.
pub const NOTIFICATIONS_MUTED_SETTINGS: &[SettingDescriptor] = &[MUTED_CHANNELS];

/// Render the Sounds and notifications groups as direct scroll anchors.
pub fn render_sounds_section<T: 'static>(
    ui: &mut SettingsPageUi,
    cx: &mut Context<T>,
) -> SettingsPage {
    let sound_settings = backend::get_sound_settings();
    let global_enabled = sound_settings.global_enabled;
    let global_volume = sound_settings.global_volume;
    let desktop_notifications = sound_settings.desktop_notifications_enabled;
    let muted_channels = sound_settings.muted_channels;
    let events = sound_settings.events;
    let live_notifications = backend::settings::get_live_notifications_enabled();
    let live_notification_delay = backend::settings::get_live_notification_startup_delay_secs();
    let notification_status = backend::notifications::desktop_notification_status();

    SettingsPage::new(SettingsSectionId::Sounds)
        .quiet()
        .subsection(
            SettingsSubsectionId::Sounds,
            render_sound_events_settings(
                &events,
                global_enabled,
                global_volume,
                desktop_notifications,
                ui,
                cx,
            ),
        )
        .subsection(
            SettingsSubsectionId::DesktopNotifications,
            render_notifications_settings(
                desktop_notifications,
                live_notifications,
                live_notification_delay,
                &notification_status,
                ui,
                cx,
            ),
        )
        .subsection(
            SettingsSubsectionId::MutedChannels,
            render_muted_channels_settings(&muted_channels, ui, cx),
        )
}

fn render_live_notification_settings<T: 'static>(
    is_enabled: bool,
    stacked: bool,
    cx: &mut Context<T>,
) -> impl IntoElement + use<T> {
    let default = settings_defaults().live_notifications_enabled;
    setting_target(
        LIVE_STATUS,
        crate::components::settings::widgets::setting_compact_row(
            LIVE_STATUS.id,
            LIVE_STATUS.label,
            Some("Add stream online and offline events to Activity Events"),
            labelled_toggle(
                SharedString::from(LIVE_STATUS.control_id),
                LIVE_STATUS.label,
                is_enabled,
                cx,
                |new_state, cx| {
                    backend::settings::set_live_notifications_enabled(new_state);
                    cx.notify();
                },
            ),
            is_enabled != default,
            (is_enabled != default).then(|| {
                setting_reset(
                    "reset-live-notifications",
                    LIVE_STATUS.label,
                    cx,
                    move |cx| {
                        backend::settings::set_live_notifications_enabled(default);
                        cx.notify();
                    },
                )
            }),
            stacked,
        ),
    )
}

fn render_live_notification_delay_row<T: 'static>(
    value: u32,
    enabled: bool,
    stacked: bool,
    cx: &mut Context<T>,
) -> impl IntoElement + use<T> {
    const MIN: u32 = 0;
    const MAX: u32 = 600;
    const STEP: u32 = 30;

    let value = value.clamp(MIN, MAX);

    let default = settings_defaults().live_notification_startup_delay_secs;
    setting_row_with_enabled(
        LIVE_STARTUP_DELAY.id,
        LIVE_STARTUP_DELAY.label,
        Some("Suppress stream online and offline notifications during startup"),
        div()
            .flex()
            .items_center()
            .justify_end()
            .gap(px(Spacing::SM))
            .child(render_step_button(
                "live-delay-dec",
                "-",
                enabled && value > MIN,
                cx,
                move |cx| {
                    let new_value = value.saturating_sub(STEP);
                    backend::settings::set_live_notification_startup_delay_secs(new_value);
                    cx.notify();
                },
            ))
            .child(
                div()
                    .w(px(72.0))
                    .text_center()
                    .text_sm()
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(colors::text_primary())
                    .child(format!("{value}s")),
            )
            .child(render_step_button(
                "live-delay-inc",
                "+",
                enabled && value < MAX,
                cx,
                move |cx| {
                    let new_value = value.saturating_add(STEP).min(MAX);
                    backend::settings::set_live_notification_startup_delay_secs(new_value);
                    cx.notify();
                },
            )),
        value != default,
        (value != default).then(|| {
            setting_reset(
                "reset-live-notification-delay",
                LIVE_STARTUP_DELAY.label,
                cx,
                move |cx| {
                    backend::settings::set_live_notification_startup_delay_secs(default);
                    cx.notify();
                },
            )
        }),
        stacked,
        SettingsMetrics::CALM_CONTROL_WIDTH,
        false,
        enabled,
    )
}

fn render_step_button<T: 'static, F: Fn(&mut Context<T>) + 'static>(
    id: &str,
    label: &'static str,
    enabled: bool,
    cx: &mut Context<T>,
    on_click: F,
) -> impl IntoElement + use<T, F> {
    let entity = cx.entity();
    let action = Rc::new(on_click);
    let activate = Rc::clone(&action);
    let accessible = Rc::clone(&action);
    let accessible_label = match label {
        "-" => "Decrease startup delay",
        "+" => "Increase startup delay",
        _ => "Adjust startup delay",
    };

    div()
        .id(SharedString::from(id.to_string()))
        .role(Role::Button)
        .map(|control| {
            crate::components::settings::widgets::settings_keyboard_control_if_enabled(
                control, enabled, cx,
            )
        })
        .a11y_synthetic_children(crate::components::ui::accessibility::apply_description)
        .aria_label(SharedString::from(accessible_label))
        .w(px(24.0))
        .h(px(24.0))
        .rounded(Radii::sm())
        .bg(colors::bg_elevated())
        .border_1()
        .border_color(colors::border())
        .cursor_pointer()
        .hover(|s| if enabled { s.bg(colors::bg_hover()) } else { s })
        .on_activate_if(
            enabled,
            cx.listener(move |_this, _event, _window, cx| {
                (*activate)(cx);
            }),
        )
        .when(enabled, |button| {
            button.on_a11y_action(AccessibleAction::Click, move |_data, _window, app| {
                entity.update(app, |_this, cx| (*accessible)(cx));
            })
        })
        .flex()
        .items_center()
        .justify_center()
        .child(
            div()
                .text_sm()
                .text_color(colors::text_secondary())
                .child(label),
        )
}

fn render_master_sound_row<T: 'static>(
    is_enabled: bool,
    volume: f32,
    stacked: bool,
    cx: &mut Context<T>,
) -> impl IntoElement + use<T> {
    let defaults = sound_defaults();
    let volume_percent = (volume * 100.0).round() as i32;

    div()
        .w_full()
        .flex()
        .flex_col()
        .child(setting_target(
            SOUND_MASTER_ENABLED,
            crate::components::settings::widgets::setting_compact_row(
                SOUND_MASTER_ENABLED.id,
                SOUND_MASTER_ENABLED.label,
                Some("Mute or restore every sound event"),
                labelled_toggle(
                    SharedString::from(SOUND_MASTER_ENABLED.control_id),
                    SOUND_MASTER_ENABLED.label,
                    is_enabled,
                    cx,
                    |new_state, cx| {
                        backend::set_global_sound_enabled(new_state);
                        cx.notify();
                    },
                ),
                is_enabled != defaults.global_enabled,
                (is_enabled != defaults.global_enabled).then(|| {
                    setting_reset(
                        "reset-global-sound",
                        SOUND_MASTER_ENABLED.label,
                        cx,
                        move |cx| {
                            backend::set_global_sound_enabled(defaults.global_enabled);
                            cx.notify();
                        },
                    )
                }),
                stacked,
            ),
        ))
        .child(setting_target(
            SOUND_MASTER_VOLUME,
            setting_row_with_enabled(
                SOUND_MASTER_VOLUME.id,
                SOUND_MASTER_VOLUME.label,
                Some("Adjust the volume used by every sound event"),
                div()
                    .w_full()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .gap(px(Spacing::SM))
                    .child(
                        div()
                            .min_w(px(128.0))
                            .flex_1()
                            .child(render_sound_volume_slider(
                                "vol-bar-master".to_string(),
                                SOUND_MASTER_VOLUME.label,
                                volume,
                                is_enabled,
                                false,
                                cx,
                                |new_volume, cx| {
                                    backend::set_global_sound_volume(new_volume);
                                    cx.notify();
                                },
                            )),
                    )
                    .child(
                        div()
                            .w(px(40.0))
                            .flex_shrink_0()
                            .text_right()
                            .text_xs()
                            .text_color(colors::text_secondary())
                            .child(format!("{volume_percent}%")),
                    ),
                setting_float_modified(volume, defaults.global_volume),
                setting_float_modified(volume, defaults.global_volume).then(|| {
                    setting_reset(
                        "reset-global-sound-volume",
                        SOUND_MASTER_VOLUME.label,
                        cx,
                        move |cx| {
                            backend::set_global_sound_volume(defaults.global_volume);
                            cx.notify();
                        },
                    )
                }),
                stacked,
                SettingsMetrics::CALM_SLIDER_CONTROL_WIDTH,
                false,
                is_enabled,
            ),
        ))
}

fn render_sound_events_settings<T: 'static>(
    events: &[backend::SoundEventSettings],
    global_enabled: bool,
    global_volume: f32,
    desktop_notifications_enabled: bool,
    ui: &mut SettingsPageUi,
    cx: &mut Context<T>,
) -> impl IntoElement + use<T> {
    let stacked = ui.stacked;
    let sound_events = ui.disclosure(SOUND_EVENTS_DISCLOSURE, cx);
    let events_modified = events
        .iter()
        .filter(|event| {
            let default = sound_event_default(event.event_type.clone());
            event.enabled != default.enabled || setting_float_modified(event.volume, default.volume)
        })
        .count();

    settings_group(
        "sounds",
        "Sounds",
        false,
        settings_group_contents(
            "sounds-content",
            "Sounds",
            true,
            div()
                .w_full()
                .min_w_0()
                .flex()
                .flex_col()
                .child(render_master_sound_row(
                    global_enabled,
                    global_volume,
                    stacked,
                    cx,
                ))
                .child(setting_target(
                    SOUND_EVENTS,
                    settings_disclosure_with_layout(
                        SOUND_EVENTS_DISCLOSURE,
                        SOUND_EVENTS.label,
                        Some("Choose the sounds used for mentions, events, and highlights"),
                        events_modified,
                        &sound_events,
                        stacked,
                        cx,
                        move |cx| {
                            render_events_list(
                                events,
                                global_enabled,
                                desktop_notifications_enabled,
                                stacked,
                                cx,
                            )
                        },
                    ),
                )),
        ),
    )
}

fn render_notifications_settings<T: 'static>(
    is_enabled: bool,
    live_notifications: bool,
    startup_delay_secs: u32,
    status: &backend::notifications::DesktopNotificationStatus,
    ui: &mut SettingsPageUi,
    cx: &mut Context<T>,
) -> impl IntoElement + use<T> {
    let stacked = ui.stacked;
    let more = ui.disclosure(NOTIFICATIONS_MORE_DISCLOSURE, cx);
    let default = sound_defaults().desktop_notifications_enabled;
    let default_delay = settings_defaults().live_notification_startup_delay_secs;
    let delay_modified = startup_delay_secs != default_delay;

    settings_group(
        "desktop-notifications",
        "Notifications",
        false,
        settings_group_contents(
            "desktop-notifications-content",
            "Notifications",
            true,
            div()
                .w_full()
                .min_w_0()
                .flex()
                .flex_col()
                .child(setting_target(
                    DESKTOP_NOTIFICATIONS,
                    render_desktop_notification_row(is_enabled, default, status, stacked, cx),
                ))
                .child(render_live_notification_settings(
                    live_notifications,
                    stacked,
                    cx,
                ))
                .child(settings_disclosure_with_layout(
                    NOTIFICATIONS_MORE_DISCLOSURE,
                    "Notification timing",
                    Some("Startup delay"),
                    usize::from(delay_modified),
                    &more,
                    stacked,
                    cx,
                    move |cx| {
                        div()
                            .w_full()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .child(settings_subheading("Startup"))
                            .child(setting_target(
                                LIVE_STARTUP_DELAY,
                                render_live_notification_delay_row(
                                    startup_delay_secs,
                                    live_notifications,
                                    stacked,
                                    cx,
                                ),
                            ))
                    },
                )),
        ),
    )
}

fn render_desktop_notification_row<T: 'static>(
    is_enabled: bool,
    default: bool,
    status: &backend::notifications::DesktopNotificationStatus,
    stacked: bool,
    cx: &mut Context<T>,
) -> impl IntoElement + use<T> {
    let modified = is_enabled != default;
    let detail = status.detail.clone();
    let detail_color = if status.supported {
        colors::text_secondary()
    } else {
        colors::warning()
    };

    div()
        .id(DESKTOP_NOTIFICATIONS.id)
        .role(Role::Group)
        .aria_label(SharedString::from(DESKTOP_NOTIFICATIONS.label))
        .aria_description(SharedString::from(detail.clone()))
        .w_full()
        .min_w_0()
        .min_h(px(SettingsMetrics::CALM_ROW_MIN_HEIGHT))
        .py(px(Spacing::SM))
        .flex()
        .gap(px(SettingsMetrics::ROW_COLUMN_GAP))
        .map(|row| {
            if stacked {
                row.flex_col().items_start()
            } else {
                row.items_center()
            }
        })
        .child(
            div()
                .min_w_0()
                .flex_1()
                .flex()
                .flex_col()
                .gap(px(SettingsMetrics::LABEL_GAP))
                .child(
                    div()
                        .flex()
                        .flex_wrap()
                        .items_center()
                        .gap(px(Spacing::SM))
                        .child(
                            div()
                                .text_size(px(SettingsMetrics::CALM_LABEL_TEXT))
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(colors::text_primary())
                                .child(DESKTOP_NOTIFICATIONS.label),
                        )
                        .when(modified, |label| {
                            label.child(
                                div()
                                    .text_size(px(SettingsMetrics::CALM_HELPER_TEXT))
                                    .text_color(colors::text_secondary())
                                    .child("Modified"),
                            )
                        }),
                )
                .child(
                    div()
                        .min_w_0()
                        .text_size(px(SettingsMetrics::CALM_HELPER_TEXT))
                        .text_color(detail_color)
                        .whitespace_normal()
                        .child(detail),
                ),
        )
        .child(
            div()
                .min_w_0()
                .flex_shrink_0()
                .map(|column| {
                    if stacked {
                        column.w_full()
                    } else {
                        column.w(px(SettingsMetrics::CALM_CONTROL_WIDTH))
                    }
                })
                .flex()
                .items_center()
                .justify_end()
                .gap(px(Spacing::SM))
                .child(
                    div()
                        .min_w_0()
                        .flex_1()
                        .flex()
                        .items_center()
                        .justify_end()
                        .gap(px(Spacing::SM))
                        .child(
                            render_secondary_action(
                                "desktop-notif-test",
                                "Test",
                                status.supported,
                                cx,
                                |cx| {
                                    backend::notifications::test_desktop_notification();
                                    cx.notify();
                                },
                            )
                            .aria_label(SharedString::from("Send test notification")),
                        )
                        .child(labelled_toggle(
                            SharedString::from(DESKTOP_NOTIFICATIONS.control_id),
                            DESKTOP_NOTIFICATIONS.label,
                            is_enabled,
                            cx,
                            |new_state, cx| {
                                backend::set_desktop_notifications_enabled(new_state);
                                cx.notify();
                            },
                        )),
                )
                .when(modified, |column| {
                    column.child(setting_reset(
                        "reset-desktop-notifications",
                        DESKTOP_NOTIFICATIONS.label,
                        cx,
                        move |cx| {
                            backend::set_desktop_notifications_enabled(default);
                            cx.notify();
                        },
                    ))
                }),
        )
}

fn render_events_list<T: 'static>(
    events: &[backend::SoundEventSettings],
    global_enabled: bool,
    desktop_notifications_enabled: bool,
    stacked: bool,
    cx: &mut Context<T>,
) -> impl IntoElement + use<T> {
    div()
        .flex()
        .flex_col()
        .gap_2()
        .children(events.iter().map(|event| {
            render_sound_event_row(
                event,
                global_enabled,
                desktop_notifications_enabled,
                stacked,
                cx,
            )
            .into_any_element()
        }))
}

fn render_muted_channels_settings<T: 'static>(
    channels: &[String],
    ui: &mut SettingsPageUi,
    cx: &mut Context<T>,
) -> impl IntoElement + use<T> {
    let has_muted_channels = !channels.is_empty();
    let muted = ui.disclosure(MUTED_CHANNELS_DISCLOSURE, cx);
    let summary = match channels.len() {
        0 => "No muted channels".to_string(),
        1 => "1 muted".to_string(),
        count => format!("{count} muted"),
    };

    settings_group(
        "muted-channels",
        "Muted channels",
        false,
        setting_target(
            MUTED_CHANNELS,
            settings_summary_disclosure(
                MUTED_CHANNELS_DISCLOSURE,
                MUTED_CHANNELS.label,
                &summary,
                &muted,
                cx,
                move |cx| {
                    div()
                    .w_full()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(px(Spacing::SM))
                    .child(
                        div()
                            .text_xs()
                            .text_color(colors::text_secondary())
                            .child("New channel mutes silence sounds and desktop notifications. Existing sound-only mutes stay sound-only until you choose Mute desktop too. Activity and visual highlights remain visible."),
                    )
                    .when(!has_muted_channels, |body| {
                        body.child(
                            div()
                                .text_sm()
                                .text_color(colors::text_secondary())
                                .child("No channels are muted. Use channel notification hotkeys to add channels here."),
                        )
                    })
                    .when(has_muted_channels, |body| {
                        body.child(
                            div()
                                .flex()
                                .justify_end()
                                .child(render_secondary_action(
                                    "clear-muted-highlight-sounds",
                                    "Unmute all",
                                    true,
                                    cx,
                                    |_cx| {
                                        backend::push_ui_action(backend::UiAction::ConfirmClearMutedSounds);
                                    },
                                )),
                        )
                    })
                    .children(
                        channels
                            .iter()
                            .map(|channel| render_muted_channel_row(channel, cx).into_any_element()),
                    )
                },
            ),
        ),
    )
}

fn render_muted_channel_row<T: 'static>(
    channel: &str,
    cx: &mut Context<T>,
) -> impl IntoElement + use<T> {
    let channel_name = channel.to_string();
    let button_id = format!("unmute-highlight-sounds-{channel}");
    let desktop_muted = backend::bridge::channel_desktop_notifications_muted(channel);
    let mute_desktop_channel = channel_name.clone();

    div()
        .w_full()
        .min_w_0()
        .flex()
        .flex_wrap()
        .items_center()
        .gap(px(Spacing::SM))
        .justify_between()
        .child(
            div()
                .flex()
                .flex_col()
                .gap_1()
                .child(
                    div()
                        .text_sm()
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(colors::text_primary())
                        .child(channel.to_string()),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(colors::text_muted())
                        .child(if desktop_muted {
                            "Sounds and desktop notifications are muted."
                        } else {
                            "Sounds are muted; desktop notifications are still allowed."
                        }),
                ),
        )
        .when(!desktop_muted, |row| {
            row.child(render_secondary_action(
                format!("mute-desktop-{channel}"),
                "Mute desktop too",
                true,
                cx,
                move |cx| {
                    backend::set_highlight_sound_channel_muted(&mute_desktop_channel, true);
                    cx.notify();
                },
            ))
        })
        .child(render_secondary_action(
            &button_id,
            "Unmute",
            true,
            cx,
            move |cx| {
                backend::set_highlight_sound_channel_muted(&channel_name, false);
                cx.notify();
            },
        ))
}

fn render_sound_event_row<T: 'static>(
    event: &backend::SoundEventSettings,
    global_enabled: bool,
    desktop_notifications_enabled: bool,
    stacked: bool,
    cx: &mut Context<T>,
) -> impl IntoElement + use<T> {
    let event_type_for_enabled = event.event_type.clone();
    let event_type_for_enabled_reset = event.event_type.clone();
    let event_type_for_test = event.event_type.clone();
    let event_type_for_volume = event.event_type.clone();
    let event_type_for_volume_reset = event.event_type.clone();
    let enabled_target = sound_event_descriptor(&event.event_type, SoundEventField::Enabled);
    let volume_target = sound_event_descriptor(&event.event_type, SoundEventField::Volume);
    let preview_target = sound_event_descriptor(&event.event_type, SoundEventField::Preview);
    let volume_slider_id = format!("vol-bar-{:?}", event.event_type);
    let feedback_target = crate::benchmark_ui_fixture::feedback_ui_enabled()
        && event.event_type == backend::SoundEventType::Mention;
    let display_name = sound_event_display_name(event.event_type.clone());
    let is_enabled = event.enabled;
    let volume = event.volume;
    let volume_percent = (volume * 100.0).round() as i32;
    let default = sound_event_default(event.event_type.clone());
    let enabled_modified = is_enabled != default.enabled;
    let volume_modified = setting_float_modified(volume, default.volume);
    let supports_desktop = supports_desktop_notification(event.event_type.clone().into());
    let status_text = sound_event_status_text(
        event,
        global_enabled,
        desktop_notifications_enabled,
        supports_desktop,
    );

    setting_row_with_enabled(
        format!("sound-event-{:?}", event.event_type),
        display_name,
        Some(&status_text),
        div()
            .w_full()
            .min_w_0()
            .flex()
            .flex_wrap()
            .items_center()
            .justify_end()
            .gap(px(Spacing::SM))
            .child(setting_target(
                enabled_target,
                toggle_with_enabled(
                    SharedString::from(format!("sound-event-{:?}-enabled", event.event_type)),
                    is_enabled,
                    global_enabled,
                    cx,
                    move |new_state, cx| {
                        backend::set_sound_event_enabled(event_type_for_enabled.clone(), new_state);
                        cx.notify();
                    },
                )
                .aria_label(SharedString::from(format!("Enable {display_name} sound"))),
            ))
            .when(enabled_modified, |controls| {
                let default = default.enabled;
                controls.child(setting_reset(
                    format!("reset-sound-event-{:?}-enabled", event.event_type),
                    &format!("{display_name} sound"),
                    cx,
                    move |cx| {
                        backend::set_sound_event_enabled(
                            event_type_for_enabled_reset.clone(),
                            default,
                        );
                        cx.notify();
                    },
                ))
            })
            .child(
                div()
                    .min_w(px(176.0))
                    .flex_1()
                    .flex()
                    .items_center()
                    .gap(px(Spacing::SM))
                    .child(div().min_w(px(128.0)).flex_1().child(setting_target(
                        volume_target,
                        render_sound_volume_slider(
                            volume_slider_id,
                            volume_target.label,
                            volume,
                            is_enabled && global_enabled,
                            feedback_target,
                            cx,
                            move |new_volume, cx| {
                                backend::set_sound_event_volume(
                                    event_type_for_volume.clone(),
                                    new_volume,
                                );
                                cx.notify();
                            },
                        ),
                    )))
                    .child(
                        div()
                            .w(px(40.0))
                            .flex_shrink_0()
                            .text_right()
                            .text_xs()
                            .text_color(colors::text_secondary())
                            .child(format!("{volume_percent}%")),
                    ),
            )
            .when(volume_modified, |controls| {
                let default = default.volume;
                controls.child(setting_reset(
                    format!("reset-sound-event-{:?}-volume", event.event_type),
                    &format!("{display_name} sound volume"),
                    cx,
                    move |cx| {
                        backend::set_sound_event_volume(
                            event_type_for_volume_reset.clone(),
                            default,
                        );
                        cx.notify();
                    },
                ))
            })
            .child(setting_target(
                preview_target,
                render_test_button(event_type_for_test, global_enabled, cx),
            )),
        enabled_modified || volume_modified,
        None,
        stacked,
        SettingsMetrics::CALM_SLIDER_CONTROL_WIDTH,
        false,
        global_enabled,
    )
}

fn sound_event_status_text(
    event: &backend::SoundEventSettings,
    global_enabled: bool,
    desktop_notifications_enabled: bool,
    supports_desktop: bool,
) -> String {
    let phrase_event = event.event_type == backend::SoundEventType::Phrase;

    if !global_enabled {
        return "Muted by the master sound setting.".to_string();
    }

    if event.enabled {
        if desktop_notifications_enabled && supports_desktop && phrase_event {
            "Sound and custom rule alerts.".to_string()
        } else if desktop_notifications_enabled && supports_desktop {
            "Sound and desktop notification.".to_string()
        } else if supports_desktop {
            "Sound only; desktop notifications are off.".to_string()
        } else {
            "Sound only.".to_string()
        }
    } else if desktop_notifications_enabled && supports_desktop && phrase_event {
        "Muted; custom rule alerts are off.".to_string()
    } else if desktop_notifications_enabled && supports_desktop {
        "Muted; desktop notifications are off.".to_string()
    } else {
        "Muted; Test uses the current volume.".to_string()
    }
}

/// Volume thumb diameter; the thumb is centered on the current value.
const VOLUME_THUMB_SIZE: f32 = 14.0;
/// Volume thumb diameter while the slider is hovered.
const VOLUME_THUMB_HOVER_SIZE: f32 = 16.0;

fn render_sound_volume_slider<T: 'static, F: Fn(f32, &mut Context<T>) + Clone + 'static>(
    slider_id: String,
    label: &'static str,
    value: f32,
    enabled: bool,
    feedback_target: bool,
    cx: &mut Context<T>,
    on_change: F,
) -> impl IntoElement + use<T, F> {
    let normalized = value.clamp(0.0, 1.0);
    let group_id = SharedString::from(format!("{slider_id}-hover"));
    let bounds = Rc::new(Cell::new(None::<Bounds<Pixels>>));
    let bounds_for_prepaint = Rc::clone(&bounds);
    let bounds_for_click = Rc::clone(&bounds);
    let bounds_for_fixture_click = Rc::clone(&bounds);
    let slider_id_for_drag = slider_id.clone();
    let slider_id_for_check = slider_id.clone();
    let on_change_for_drag = on_change.clone();
    let on_change_for_fixture_click = on_change.clone();
    let on_change_for_keys = on_change.clone();
    let on_change_for_click = on_change;

    div()
        .on_children_prepainted(move |child_bounds, _window, _cx| {
            let slider_bounds = child_bounds.first().copied();
            bounds_for_prepaint.set(slider_bounds);
            if feedback_target && let Some(bounds) = slider_bounds {
                crate::benchmark_ui_fixture::publish_feedback_sound_slider(
                    crate::benchmark_ui_fixture::FeedbackRect {
                        x: bounds.origin.x.into(),
                        y: bounds.origin.y.into(),
                        width: bounds.size.width.into(),
                        height: bounds.size.height.into(),
                    },
                    normalized,
                    enabled,
                );
            }
        })
        .id(SharedString::from(slider_id))
        .group(group_id.clone())
        .w_full()
        .h(px(18.0))
        .relative()
        .cursor_pointer()
        .when(!enabled, |slider| slider.cursor_default())
        .when(enabled, |slider| {
            crate::components::settings::widgets::slider_keyboard(
                slider,
                label,
                normalized,
                0.0,
                1.0,
                0.05,
                format!("{}%", (normalized * 100.0).round() as i32),
                cx,
                on_change_for_keys,
            )
        })
        .when(feedback_target, |slider| {
            slider
                .on_hover(|hovered, _window, _cx| {
                    crate::benchmark_ui_fixture::set_feedback_sound_slider_hovered(*hovered);
                })
                .on_mouse_move(|_event, _window, _cx| {
                    crate::benchmark_ui_fixture::set_feedback_sound_slider_hovered(true);
                })
        })
        .when(enabled && !feedback_target, |slider| {
            slider
                .on_drag(
                    SliderDrag {
                        slider_id: slider_id_for_drag,
                    },
                    |drag, _, _, cx| {
                        cx.stop_propagation();
                        cx.new(|_| drag.clone())
                    },
                )
                .on_drag_move(cx.listener(
                    move |_this, event: &DragMoveEvent<SliderDrag>, _window, cx| {
                        if event.drag(cx).slider_id != slider_id_for_check {
                            return;
                        }
                        let relative_x: f32 =
                            (event.event.position.x - event.bounds.origin.x).into();
                        let width: f32 = event.bounds.size.width.into();
                        let new_volume = (relative_x / width.max(1.0)).clamp(0.0, 1.0);
                        on_change_for_drag(new_volume, cx);
                        if feedback_target {
                            crate::benchmark_ui_fixture::set_feedback_sound_slider_clicked(
                                new_volume,
                            );
                        }
                    },
                ))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |_this, event: &MouseDownEvent, _window, cx| {
                        cx.stop_propagation();
                        let Some(bounds) = bounds_for_click.get() else {
                            return;
                        };
                        let relative_x: f32 = (event.position.x - bounds.origin.x).into();
                        let width: f32 = bounds.size.width.into();
                        let new_volume = (relative_x / width.max(1.0)).clamp(0.0, 1.0);
                        on_change_for_click(new_volume, cx);
                        if feedback_target {
                            crate::benchmark_ui_fixture::set_feedback_sound_slider_clicked(
                                new_volume,
                            );
                        }
                    }),
                )
        })
        .child(
            div()
                .w_full()
                .h_full()
                .relative()
                .child(
                    div()
                        .absolute()
                        .left_0()
                        .right_0()
                        .top(px(6.0))
                        .h(px(6.0))
                        .rounded_full()
                        .bg(colors::bg_elevated())
                        .group_hover(group_id.clone(), |style| style.bg(colors::border())),
                )
                .child(
                    div()
                        .absolute()
                        .left_0()
                        .top(px(6.0))
                        .h(px(6.0))
                        .w(relative(normalized))
                        .rounded_full()
                        .bg(if enabled {
                            colors::accent()
                        } else {
                            colors::text_muted()
                        }),
                )
                .child(
                    div()
                        .absolute()
                        .left(relative(normalized))
                        .top(px(2.0))
                        .ml(px(-VOLUME_THUMB_SIZE / 2.0))
                        .size(px(VOLUME_THUMB_SIZE))
                        .rounded_full()
                        .bg(colors::text_on_accent())
                        .border_2()
                        .border_color(if enabled {
                            colors::accent()
                        } else {
                            colors::text_muted()
                        })
                        .shadow_sm()
                        .group_hover(group_id, |style| {
                            style
                                .top(px(1.0))
                                .ml(px(-VOLUME_THUMB_HOVER_SIZE / 2.0))
                                .size(px(VOLUME_THUMB_HOVER_SIZE))
                                .shadow_md()
                        }),
                ),
        )
        .when(feedback_target, |slider| {
            slider.child(
                div()
                    .id("feedback-sound-slider-input-layer")
                    .absolute()
                    .inset_0()
                    .cursor_pointer()
                    .on_hover(|hovered, _window, _cx| {
                        crate::benchmark_ui_fixture::set_feedback_sound_slider_hovered(*hovered);
                    })
                    .on_mouse_move(|_event, _window, _cx| {
                        crate::benchmark_ui_fixture::set_feedback_sound_slider_hovered(true);
                    })
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |_this, event: &MouseDownEvent, _window, cx| {
                            if !enabled {
                                return;
                            }
                            cx.stop_propagation();
                            let Some(bounds) = bounds_for_fixture_click.get() else {
                                return;
                            };
                            let relative_x: f32 = (event.position.x - bounds.origin.x).into();
                            let width: f32 = bounds.size.width.into();
                            let new_volume = (relative_x / width.max(1.0)).clamp(0.0, 1.0);
                            on_change_for_fixture_click(new_volume, cx);
                            crate::benchmark_ui_fixture::set_feedback_sound_slider_clicked(
                                new_volume,
                            );
                        }),
                    ),
            )
        })
}

fn render_test_button<T: 'static>(
    event_type: backend::SoundEventType,
    enabled: bool,
    cx: &mut Context<T>,
) -> impl IntoElement + use<T> {
    let accessible_label = sound_event_descriptor(&event_type, SoundEventField::Preview).label;
    render_secondary_action(
        format!("test-{:?}", event_type),
        "Test",
        enabled,
        cx,
        move |cx| {
            backend::test_sound(event_type.clone());
            cx.notify();
        },
    )
    .aria_label(SharedString::from(accessible_label))
}

fn render_secondary_action<T: 'static, F: Fn(&mut Context<T>) + 'static>(
    id: impl Into<SharedString>,
    label: &str,
    enabled: bool,
    cx: &mut Context<T>,
    on_click: F,
) -> Stateful<Div> {
    let entity = cx.entity();
    let action = Rc::new(on_click);
    let activate = Rc::clone(&action);
    let accessible = Rc::clone(&action);

    div()
        .id(id.into())
        .role(Role::Button)
        .map(|control| {
            crate::components::settings::widgets::settings_keyboard_control_if_enabled(
                control, enabled, cx,
            )
        })
        .a11y_synthetic_children(crate::components::ui::accessibility::apply_description)
        .aria_label(SharedString::from(label.to_string()))
        .px(px(Spacing::SM))
        .py(px(Spacing::XS))
        .min_h(px(24.0))
        .rounded(Radii::sm())
        .bg(colors::bg_elevated())
        .border_1()
        .border_color(colors::border())
        .cursor_pointer()
        .hover(|style| style.bg(colors::bg_hover()))
        .text_size(px(SettingsMetrics::CALM_HELPER_TEXT))
        .text_color(colors::text_primary())
        .child(label.to_string())
        .on_activate_if(
            enabled,
            cx.listener(move |_this, _event, _window, cx| {
                (*activate)(cx);
            }),
        )
        .when(enabled, |button| {
            button.on_a11y_action(AccessibleAction::Click, move |_data, _window, app| {
                entity.update(app, |_this, cx| (*accessible)(cx));
            })
        })
}
