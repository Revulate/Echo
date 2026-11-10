//! Settings sections for Echo GPUI.
//!
//! Every page renderer returns a [`SettingsPage`] — the direct anchor blocks the
//! scrolling content pane spreads with `.children(page.into_blocks())`. Nesting
//! those blocks inside another container collapses the `ScrollHandle` child list
//! and breaks both anchor jumps and scroll-spy, so hosts must spread them.

mod chatterino_import;
mod commands;
mod debug;
mod embeds;
mod emotes;
mod filters;
mod general;
mod highlights;
mod hotkeys;
mod logging;
mod moderation;
mod nicknames;
mod privacy;
mod proxy;
mod similarity;
mod sounds;
mod theme;
mod updates;
mod uploader;

pub use commands::*;
pub use debug::*;
pub use embeds::*;
pub use emotes::*;
pub use filters::*;
pub use general::*;
pub use highlights::*;
pub use hotkeys::*;
pub use logging::*;
pub use moderation::*;
pub use nicknames::*;
pub use privacy::*;
pub use proxy::*;
pub use similarity::*;
pub use sounds::*;
pub use theme::*;
pub use updates::*;
pub use uploader::*;

use crate::components::auth::LoginModalState;
use crate::components::settings::settings_navigation::{
    SettingDescriptor, SettingsSectionId, SettingsSubsectionId, canonical_section,
};
use crate::components::settings::settings_page::SettingsPage;
use crate::components::settings::widgets::{button, settings_group, settings_group_contents};
use crate::theme::colors;
use gpui::prelude::FluentBuilder;
use gpui::{AnyElement, Context, Div, Entity, EntityId, ParentElement, Styled, Window, div};

const fn combine_settings<const N: usize>(
    groups: &[&[SettingDescriptor]],
) -> [SettingDescriptor; N] {
    let mut combined = [groups[0][0]; N];
    let mut output = 0;
    let mut group = 0;
    while group < groups.len() {
        let mut item = 0;
        while item < groups[group].len() {
            combined[output] = groups[group][item];
            output += 1;
            item += 1;
        }
        group += 1;
    }
    combined
}

pub const CHAT_READING_ALL_SETTINGS: &[SettingDescriptor] = &combine_settings::<
    {
        CHAT_READING_SETTINGS.len()
            + CHAT_READING_NAME_SETTINGS.len()
            + CHAT_MORE_NAME_SETTINGS.len()
            + CHAT_READING_MORE_SETTINGS.len()
            + CHAT_MORE_MESSAGE_DISPLAY_SETTINGS.len()
    },
>(&[
    CHAT_READING_NAME_SETTINGS,
    CHAT_READING_SETTINGS,
    CHAT_MORE_NAME_SETTINGS,
    CHAT_READING_MORE_SETTINGS,
    CHAT_MORE_MESSAGE_DISPLAY_SETTINGS,
]);
pub const CHAT_COMPOSING_ALL_SETTINGS: &[SettingDescriptor] = &combine_settings::<
    { CHAT_COMPOSING_PRIMARY_SETTINGS.len() + CHAT_COMPOSING_MORE_SETTINGS.len() },
>(&[
    CHAT_COMPOSING_PRIMARY_SETTINGS,
    CHAT_COMPOSING_MORE_SETTINGS,
]);
pub const CHAT_EMOTES_ALL_SETTINGS: &[SettingDescriptor] = &combine_settings::<
    {
        CHAT_EMOTES_PRIMARY_SETTINGS.len()
            + CHAT_EMOTES_MORE_SETTINGS.len()
            + CHAT_EMOTE_RULES_SETTINGS.len()
    },
>(&[
    CHAT_EMOTES_PRIMARY_SETTINGS,
    CHAT_EMOTES_MORE_SETTINGS,
    CHAT_EMOTE_RULES_SETTINGS,
]);
pub const CHAT_OPTIONS_SETTINGS: &[SettingDescriptor] = NICKNAMES_SETTINGS;

pub const MODERATION_BUTTONS_ALL_SETTINGS: &[SettingDescriptor] = &combine_settings::<
    { MODERATION_BUTTONS_SETTINGS.len() + MODERATION_VARIABLES_SETTINGS.len() },
>(&[
    MODERATION_BUTTONS_SETTINGS,
    MODERATION_VARIABLES_SETTINGS,
]);

pub const SHORTCUTS_ALL_SETTINGS: &[SettingDescriptor] = &combine_settings::<
    {
        HOTKEYS_NAVIGATION_SETTINGS.len()
            + HOTKEYS_EDITOR_SETTINGS.len()
            + HOTKEYS_CHAT_SETTINGS.len()
            + HOTKEYS_SEARCH_SETTINGS.len()
            + HOTKEYS_MODERATION_SETTINGS.len()
            + HOTKEYS_OTHER_SETTINGS.len()
            + HOTKEYS_CUSTOM_SETTINGS.len()
    },
>(&[
    HOTKEYS_EDITOR_SETTINGS,
    HOTKEYS_NAVIGATION_SETTINGS,
    HOTKEYS_CHAT_SETTINGS,
    HOTKEYS_SEARCH_SETTINGS,
    HOTKEYS_MODERATION_SETTINGS,
    HOTKEYS_OTHER_SETTINGS,
    HOTKEYS_CUSTOM_SETTINGS,
]);

pub const APPEARANCE_OPTIONS_SETTINGS: &[SettingDescriptor] = &combine_settings::<
    { APPEARANCE_THEME_OPTIONS_SETTINGS.len() + APPEARANCE_WINDOWS_OVERLAY_SETTINGS.len() },
>(&[
    APPEARANCE_THEME_OPTIONS_SETTINGS,
    APPEARANCE_WINDOWS_OVERLAY_SETTINGS,
]);

/// Developer Tools › Diagnostics.
///
/// The only anchor fed by two modules: `logging.rs` owns the metric copy/reset
/// actions and `debug.rs` owns the live readouts. They are stitched together
/// here so the navigation tables still see exactly one slice per anchor.
pub const DEVELOPER_DIAGNOSTICS_SETTINGS: &[SettingDescriptor] = &[
    logging::DIAGNOSTICS_JSON,
    logging::DIAGNOSTICS_RESET,
    debug::FRAME_RATE_ANALYSIS,
    debug::RUNTIME_LIVENESS,
    debug::SEVENTV_EVENTAPI,
    debug::MESSAGE_PERFORMANCE,
    debug::EMOTE_LOADING,
    debug::MEMORY_USAGE,
    debug::DEBUG_ACTIONS,
];

/// The last line of a regex compile error: the reason, without the echoed
/// pattern and caret lines, so it fits a one-line status.
fn brief_regex_error(error: &regex::Error) -> String {
    let message = error.to_string();
    message
        .lines()
        .rev()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("invalid pattern")
        .to_string()
}

// Every per-host editor input and viewer state a section keeps outside the
// surface is released from the two functions below, so a new editor registers
// its cleanup here rather than in the surface.

/// Release all per-host section state when a settings surface closes or drops.
pub(crate) fn release_host_editor_state(host: EntityId) {
    general::release_player_path_inputs_for_host(host);
    proxy::release_proxy_inputs_for_host(host);
    logging::forget_logging_viewer_state_for_host(host);
    commands::release_command_editor_inputs_for_host(host);
    hotkeys::forget_hotkey_editor_host(host);
    uploader::clear_uploader_status(host.as_u64());
}

/// Release the inputs of pages that are not on screen, so a hidden page does
/// not keep its editor entities alive. `previous` is the page rendered last.
pub(crate) fn release_hidden_page_inputs(
    host: EntityId,
    previous: Option<SettingsSectionId>,
    section: SettingsSectionId,
) {
    if previous == Some(SettingsSectionId::Advanced) && section != SettingsSectionId::Advanced {
        proxy::release_proxy_inputs_for_host(host);
        logging::release_logging_inputs_for_host(host);
    }
    if section != SettingsSectionId::Hotkeys {
        commands::release_command_editor_inputs_for_host(host);
        hotkeys::release_hotkey_editor_inputs_for_host(host);
    }
    if section != SettingsSectionId::Connections {
        general::release_player_path_inputs_for_host(host);
    }
}

/// Invalidate transient drafts whose persisted authority was replaced by a
/// successful full-settings import or reset.
pub(crate) fn reset_migrated_editor_drafts_after_settings_replace() {
    general::reset_settings_import_editor_after_settings_replace();
    general::reset_player_paths_editor_after_settings_replace();
    general::reset_timestamp_builder_after_settings_replace();
    proxy::reset_proxy_editor_after_settings_replace();
    commands::reset_command_editor_after_settings_replace();
    hotkeys::reset_custom_hotkey_editor_after_settings_replace();
    nicknames::reset_nickname_editor_after_settings_replace();
    filters::reset_filter_editor_after_settings_replace();
    highlights::reset_highlight_editors_after_settings_replace();
    moderation::reset_moderation_editor_after_settings_replace();
    similarity::reset_similarity_editor_after_settings_replace();
    uploader::reset_uploader_editor_after_settings_replace();
}

/// Build the page for `section`.
///
/// Single entry point for both hosts, so the modal and the popout can never
/// drift on which renderer owns a destination. The returned page carries the
/// anchor order the tree, breadcrumb, and scroll-spy all index against.
pub fn build_settings_page<T: 'static>(
    section: SettingsSectionId,
    login_modal_state: Option<Entity<LoginModalState>>,
    uploader_inputs: Option<&UploaderEditorInputs>,
    developer_visible: bool,
    ui: &mut super::widgets::SettingsPageUi,
    window: &mut Window,
    cx: &mut Context<T>,
) -> SettingsPage {
    let section = canonical_section(section);

    let page = match section {
        SettingsSectionId::General => {
            let mut account = render_account_section(login_modal_state, ui, cx);
            let mut privacy = render_privacy_section(ui, cx);
            let mut updates = render_updates_section(ui, cx);

            SettingsPage::new(SettingsSectionId::General)
                .subsection(
                    SettingsSubsectionId::Account,
                    take_leaf(&mut account, "account"),
                )
                .subsection(
                    SettingsSubsectionId::StreamerPrivacy,
                    take_anchor(&mut privacy, SettingsSubsectionId::StreamerPrivacy),
                )
                .subsection(
                    SettingsSubsectionId::UpdateChecks,
                    take_anchor(&mut updates, SettingsSubsectionId::UpdateChecks),
                )
                .subsection(
                    SettingsSubsectionId::SettingsWindow,
                    render_settings_window_section(ui, cx),
                )
        }
        SettingsSectionId::Theme => render_appearance_section(ui, cx),
        SettingsSectionId::ChatBehavior => {
            let names = render_chat_username_style(ui, cx);
            let reading = render_chat_reading(ui, cx);
            let composing = render_chat_composing(ui, cx);
            let emotes = render_chat_emotes_primary(ui, cx);
            let reading_more = render_chat_reading_more(ui, window, cx);
            let composing_more = render_chat_composing_more(ui, cx);
            let emotes_more = render_chat_emotes_more(ui, cx);
            let emote_rules = render_emote_display_rules(ui, cx);
            let nicknames = render_nicknames_section(ui, window, cx);
            SettingsPage::new(SettingsSectionId::ChatBehavior)
                .subsection(
                    SettingsSubsectionId::ChatReading,
                    settings_group(
                        "chat-reading",
                        "Reading",
                        false,
                        settings_group_contents(
                            "chat-reading-rows",
                            "Reading",
                            true,
                            div()
                                .w_full()
                                .min_w_0()
                                .flex()
                                .flex_col()
                                .child(names)
                                .child(reading)
                                .child(reading_more),
                        ),
                    ),
                )
                .subsection(
                    SettingsSubsectionId::ChatComposing,
                    settings_group(
                        "chat-composing",
                        "Composing",
                        false,
                        settings_group_contents(
                            "chat-composing-rows",
                            "Composing",
                            true,
                            div()
                                .w_full()
                                .min_w_0()
                                .flex()
                                .flex_col()
                                .child(composing)
                                .child(composing_more),
                        ),
                    ),
                )
                .subsection(
                    SettingsSubsectionId::EmotesDisplay,
                    settings_group(
                        "chat-emotes",
                        "Emotes",
                        false,
                        div()
                            .w_full()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .child(emotes)
                            .child(emotes_more)
                            .child(emote_rules),
                    ),
                )
                .subsection(SettingsSubsectionId::ChatOptions, nicknames)
        }
        SettingsSectionId::Highlights => {
            let mut highlights = render_highlights_section(ui, window, cx);
            let mut sounds = render_sounds_section(ui, cx);
            SettingsPage::new(SettingsSectionId::Highlights)
                .subsection(
                    SettingsSubsectionId::HighlightsBehavior,
                    take_anchor(&mut highlights, SettingsSubsectionId::HighlightsBehavior),
                )
                .subsection(
                    SettingsSubsectionId::HighlightsCustomRules,
                    take_anchor(&mut highlights, SettingsSubsectionId::HighlightsCustomRules),
                )
                .subsection(
                    SettingsSubsectionId::HighlightsPreviewColors,
                    take_anchor(
                        &mut highlights,
                        SettingsSubsectionId::HighlightsPreviewColors,
                    ),
                )
                .subsection(
                    SettingsSubsectionId::Sounds,
                    take_anchor(&mut sounds, SettingsSubsectionId::Sounds),
                )
                .subsection(
                    SettingsSubsectionId::DesktopNotifications,
                    take_anchor(&mut sounds, SettingsSubsectionId::DesktopNotifications),
                )
                .subsection(
                    SettingsSubsectionId::MutedChannels,
                    take_anchor(&mut sounds, SettingsSubsectionId::MutedChannels),
                )
        }
        SettingsSectionId::Moderation => {
            let mut moderation = render_moderation_section(ui, window, cx);
            let mut filters = render_filters_section(ui, window, cx);

            SettingsPage::new(SettingsSectionId::Moderation)
                .subsection(
                    SettingsSubsectionId::UserCard,
                    take_anchor(&mut moderation, SettingsSubsectionId::UserCard),
                )
                .subsection(
                    SettingsSubsectionId::ModerationButtons,
                    take_anchor(&mut moderation, SettingsSubsectionId::ModerationButtons),
                )
                .subsection(
                    SettingsSubsectionId::DeletedMessages,
                    render_deleted_messages_section(ui, cx),
                )
                .subsection(
                    SettingsSubsectionId::FilterRules,
                    take_anchor(&mut filters, SettingsSubsectionId::FilterRules),
                )
                .subsection(
                    SettingsSubsectionId::BlockedUsers,
                    take_anchor(&mut filters, SettingsSubsectionId::BlockedUsers),
                )
                .subsection(
                    SettingsSubsectionId::SimilarMessages,
                    take_anchor(&mut filters, SettingsSubsectionId::SimilarMessages),
                )
        }
        SettingsSectionId::Connections => match uploader_inputs {
            Some(inputs) => render_connections_section(inputs, ui, window, cx),
            None => SettingsPage::new(SettingsSectionId::Connections).subsection(
                SettingsSubsectionId::Links,
                div()
                    .text_color(colors::text_muted())
                    .child("Links and media inputs are unavailable. Reopen settings to try again."),
            ),
        },
        SettingsSectionId::Hotkeys => {
            let mut commands = render_commands_section(ui, window, cx);
            let mut hotkeys = render_hotkeys_section(ui, window, cx);

            SettingsPage::new(SettingsSectionId::Hotkeys)
                .quiet()
                .subsection(
                    SettingsSubsectionId::SlashCommands,
                    take_anchor(&mut commands, SettingsSubsectionId::SlashCommands),
                )
                .subsection(
                    SettingsSubsectionId::CustomCommands,
                    take_anchor(&mut commands, SettingsSubsectionId::CustomCommands),
                )
                .subsection(
                    SettingsSubsectionId::HotkeysNavigation,
                    take_anchor(&mut hotkeys, SettingsSubsectionId::HotkeysNavigation),
                )
        }
        SettingsSectionId::Advanced => render_advanced_section(developer_visible, ui, window, cx),
        SettingsSectionId::Sounds
        | SettingsSectionId::Updates
        | SettingsSectionId::Emotes
        | SettingsSectionId::Filters
        | SettingsSectionId::Nicknames
        | SettingsSectionId::Commands
        | SettingsSectionId::Privacy => unreachable!("legacy section must be canonicalized"),
    };
    if matches!(
        section,
        SettingsSectionId::General
            | SettingsSectionId::Theme
            | SettingsSectionId::ChatBehavior
            | SettingsSectionId::Highlights
            | SettingsSectionId::Moderation
            | SettingsSectionId::Connections
    ) {
        page.quiet()
    } else {
        page
    }
}

fn take_anchor(page: &mut SettingsPage, subsection: SettingsSubsectionId) -> AnyElement {
    page.take_subsection(subsection)
        .unwrap_or_else(|| panic!("settings anchor {subsection:?} must exist"))
}

fn take_leaf(page: &mut SettingsPage, name: &str) -> AnyElement {
    page.take_leaf()
        .unwrap_or_else(|| panic!("settings leaf {name} must exist"))
}

pub(super) fn single_line_text(text: &str) -> String {
    text.chars()
        .filter(|ch| *ch != '\r' && *ch != '\n')
        .collect()
}

/// Append the Save/Add, Cancel-while-editing, and status line controls of a
/// list editor to `row`.
pub(super) fn render_editor_footer<T: 'static>(
    row: Div,
    editing: bool,
    editor_needs_review: bool,
    status: Option<String>,
    on_save: impl Fn(&mut Context<T>) + 'static,
    reset_editor: fn(),
    cx: &mut Context<T>,
) -> Div {
    row.child(button(
        if editing { "Save" } else { "Add" },
        true,
        on_save,
        cx,
    ))
    .when(editing, |row| {
        row.child(button(
            "Cancel",
            false,
            move |cx| {
                reset_editor();
                cx.notify();
            },
            cx,
        ))
    })
    .when_some(status, |row, status| {
        row.child(
            div()
                .text_xs()
                .text_color(if editor_needs_review {
                    colors::error()
                } else {
                    colors::text_muted()
                })
                .child(status),
        )
    })
}
