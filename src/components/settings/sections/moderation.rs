//! Moderation settings section.

use super::render_editor_footer;
use crate::backend::{self, mod_buttons::ModButtonConfig};
use crate::components::settings::draft_input::{DraftField, DraftInput};
use crate::components::settings::editor_state::{EditorSnapshot, EditorState};
use crate::components::settings::settings_navigation::{
    SettingDescriptor, SettingsSectionId, SettingsSubsectionId,
};
use crate::components::settings::settings_page::SettingsPage;
use crate::components::settings::widgets::{
    SettingsPageUi, button as render_button, empty_state, labelled_toggle, quiet_action_button,
    setting_reset, setting_target, settings_defaults, settings_group, settings_group_contents,
    settings_subheading, settings_summary_disclosure,
};
use crate::theme::colors;
use crate::theme::tokens::{Radii, RadiiPixels};
use gpui::prelude::FluentBuilder;
use gpui::*;
use std::sync::LazyLock;

const SHOW_PRONOUNS: SettingDescriptor = SettingDescriptor {
    id: "moderation.show-pronouns",
    label: "Show pronouns",
    aliases: &["user card", "pronoun", "alejo"],
    control_id: "user-card-show-pronouns",
    disclosures: &[],
};

const SHOW_FOLLOW_DATE: SettingDescriptor = SettingDescriptor {
    id: "moderation.show-follow-date",
    label: "Show follow date",
    aliases: &["user card", "followed at", "following since"],
    control_id: "user-card-show-follow-date",
    disclosures: &[],
};

const SHOW_FOLLOWER_COUNT: SettingDescriptor = SettingDescriptor {
    id: "moderation.show-follower-count",
    label: "Show follower count",
    aliases: &["user card", "followers", "total"],
    control_id: "user-card-show-follower-count",
    disclosures: &[],
};

/// Moderation › User card.
pub const MODERATION_USER_CARD_SETTINGS: &[SettingDescriptor] =
    &[SHOW_PRONOUNS, SHOW_FOLLOW_DATE, SHOW_FOLLOWER_COUNT];

const MOD_BUTTON_LABEL: SettingDescriptor = SettingDescriptor {
    id: "moderation.button-label",
    label: "Moderation button label",
    aliases: &["moderation action label", "user card action label"],
    control_id: "mod-button-label",
    disclosures: &[MODERATION_BUTTONS_DISCLOSURE],
};

const MOD_BUTTON_COMMAND: SettingDescriptor = SettingDescriptor {
    id: "moderation.button-command",
    label: "Moderation button command",
    aliases: &["moderation action command", "user card action command"],
    control_id: "mod-button-command",
    disclosures: &[MODERATION_BUTTONS_DISCLOSURE],
};

const MOD_BUTTON_DESTRUCTIVE: SettingDescriptor = SettingDescriptor {
    id: "moderation.button-destructive",
    label: "Destructive moderation button",
    aliases: &["confirm destructive moderation action"],
    control_id: "mod-button-destructive",
    disclosures: &[MODERATION_BUTTONS_DISCLOSURE],
};

/// Moderation › Moderation buttons.
pub const MODERATION_BUTTONS_SETTINGS: &[SettingDescriptor] =
    &[MOD_BUTTON_LABEL, MOD_BUTTON_COMMAND, MOD_BUTTON_DESTRUCTIVE];

const MOD_VARIABLES: SettingDescriptor = SettingDescriptor {
    id: "moderation.variables",
    label: "Message variables",
    aliases: &[
        "{user.name}",
        "{channel.name}",
        "{msg.id}",
        "{msg.text}",
        "placeholder",
    ],
    control_id: "moderation-variables",
    disclosures: &[MODERATION_BUTTONS_DISCLOSURE],
};

/// Moderation › Variables.
pub const MODERATION_VARIABLES_SETTINGS: &[SettingDescriptor] = &[MOD_VARIABLES];

const MODERATION_BUTTONS_DISCLOSURE: &str = "moderation-buttons-editor";

#[derive(Clone, Copy, PartialEq, Eq)]
enum ModButtonEditorField {
    Label,
    Command,
}

#[derive(Clone, Default)]
struct ModButtonEditorState {
    label: String,
    command: String,
    icon: Option<String>,
    editing_index: Option<usize>,
    is_destructive: bool,
    status: Option<String>,
    focused: Option<ModButtonEditorField>,
}

static MOD_BUTTON_EDITOR: LazyLock<EditorState<ModButtonEditorState>> =
    LazyLock::new(|| EditorState::new("moderation buttons", ModButtonEditorState::default()));

/// Render the Moderation page's user-card and button anchors.
pub fn render_moderation_section<T: 'static>(
    ui: &mut SettingsPageUi,
    window: &mut Window,
    cx: &mut Context<T>,
) -> SettingsPage {
    let buttons = backend::settings::get_mod_buttons();
    let editor = editor_snapshot();
    let show_pronouns = backend::settings::get_show_pronouns();
    let show_follow_date = backend::settings::get_show_follow_date();
    let show_follower_count = backend::settings::get_show_follower_count();
    let defaults = settings_defaults();

    SettingsPage::new(SettingsSectionId::Moderation)
        .subsection(
            SettingsSubsectionId::UserCard,
            settings_group(
                "moderation-user-cards",
                "User cards",
                false,
                settings_group_contents(
                    "moderation-user-card-settings",
                    "User cards",
                    true,
                    div()
                        .w_full()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .child(render_user_card_toggle(
                            SHOW_PRONOUNS,
                            "Show cached pronouns on user cards when available.",
                            show_pronouns,
                            defaults.show_pronouns,
                            ui.stacked,
                            cx,
                            backend::settings::set_show_pronouns,
                        ))
                        .child(render_user_card_toggle(
                            SHOW_FOLLOW_DATE,
                            "Show the followed-at date when Twitch returns it.",
                            show_follow_date,
                            defaults.show_follow_date,
                            ui.stacked,
                            cx,
                            backend::settings::set_show_follow_date,
                        ))
                        .child(render_user_card_toggle(
                            SHOW_FOLLOWER_COUNT,
                            "Show the follower total when Twitch returns it.",
                            show_follower_count,
                            defaults.show_follower_count,
                            ui.stacked,
                            cx,
                            backend::settings::set_show_follower_count,
                        )),
                ),
            ),
        )
        .subsection(
            SettingsSubsectionId::ModerationButtons,
            render_moderation_buttons_block(buttons, editor, ui, window, cx),
        )
}

fn render_user_card_toggle<T: 'static, F: Fn(bool) + 'static>(
    descriptor: SettingDescriptor,
    help: &'static str,
    value: bool,
    default: bool,
    stacked: bool,
    cx: &mut Context<T>,
    setter: F,
) -> impl IntoElement + use<T, F> {
    let setter = std::rc::Rc::new(setter);
    let on_toggle = setter.clone();
    let on_reset = setter.clone();
    let modified = value != default;
    setting_target(
        descriptor,
        crate::components::settings::widgets::setting_compact_row(
            descriptor.id,
            descriptor.label,
            Some(help),
            labelled_toggle(
                descriptor.control_id,
                descriptor.label,
                value,
                cx,
                move |enabled, cx| {
                    on_toggle(enabled);
                    cx.notify();
                },
            ),
            modified,
            modified.then(|| {
                setting_reset(
                    format!("reset-{}", descriptor.control_id),
                    descriptor.label,
                    cx,
                    move |cx| {
                        on_reset(default);
                        cx.notify();
                    },
                )
            }),
            stacked,
        ),
    )
}

fn render_moderation_buttons_block<T: 'static>(
    buttons: Vec<ModButtonConfig>,
    editor: EditorSnapshot<ModButtonEditorState>,
    ui: &mut SettingsPageUi,
    window: &mut Window,
    cx: &mut Context<T>,
) -> impl IntoElement + use<T> {
    let summary = moderation_buttons_summary(buttons.len(), &editor);
    let disclosure = ui.disclosure(MODERATION_BUTTONS_DISCLOSURE, cx);
    if !disclosure.is_expanded() && editor.draft.focused.is_some() {
        update_editor(|editor| editor.focused = None);
    }
    let stacked = ui.stacked;
    let fields = ModButtonTextFields {
        label: mod_button_input(
            MOD_BUTTON_LABEL.control_id,
            ModButtonEditorField::Label,
            &editor.draft.label,
            "Label, for example 10m",
        )
        .escape_focus(disclosure.clone())
        .prepare(window, cx),
        command: mod_button_input(
            MOD_BUTTON_COMMAND.control_id,
            ModButtonEditorField::Command,
            &editor.draft.command,
            r#"Command, for example /timeout {user.name} 600"#,
        )
        .monospace(true)
        .escape_focus(disclosure.clone())
        .prepare(window, cx),
    };
    settings_group(
        "moderation-buttons",
        "Moderation buttons",
        false,
        settings_summary_disclosure(
            MODERATION_BUTTONS_DISCLOSURE,
            "Moderation buttons",
            &summary,
            &disclosure,
            cx,
            move |cx| render_moderation_buttons_body(buttons, editor, fields, stacked, cx),
        ),
    )
}

fn moderation_buttons_summary(
    count: usize,
    editor: &EditorSnapshot<ModButtonEditorState>,
) -> String {
    let buttons = match count {
        1 => "1 button".to_string(),
        count => format!("{count} buttons"),
    };
    let draft = &editor.draft;
    let has_draft = !draft.label.trim().is_empty()
        || !draft.command.trim().is_empty()
        || draft.icon.is_some()
        || draft.is_destructive;
    if editor.needs_review {
        format!("{buttons} · Needs review")
    } else if draft.editing_index.is_some() {
        format!("{buttons} · Editing…")
    } else if has_draft {
        format!("{buttons} · Unsaved draft")
    } else {
        buttons
    }
}

/// Text inputs for the button editor, prepared while the window is available.
struct ModButtonTextFields<T> {
    label: DraftField<T>,
    command: DraftField<T>,
}

fn render_moderation_buttons_body<T: 'static>(
    buttons: Vec<ModButtonConfig>,
    editor: EditorSnapshot<ModButtonEditorState>,
    fields: ModButtonTextFields<T>,
    stacked: bool,
    cx: &mut Context<T>,
) -> impl IntoElement + use<T> {
    let editor_generation = editor.generation;
    let editor_needs_review = editor.needs_review;
    let editor = editor.draft;
    div()
        .w_full()
        .min_w_0()
        .flex()
        .flex_col()
        .child(settings_group_contents(
            "moderation-buttons-editor-content",
            "Moderation buttons",
            false,
            div()
                .w_full()
                .min_w_0()
                .flex()
                .flex_col()
                .gap_4()
                .child(
                    div()
                        .max_w(px(640.0))
                        .text_xs()
                        .text_color(colors::text_muted())
                        .whitespace_normal()
                        .child("Add commands shown on user cards. Message variables are substituted when available."),
                )
                .child(render_editor(
                    editor,
                    editor_generation,
                    editor_needs_review,
                    fields,
                    stacked,
                    cx,
                ))
                .child(render_button_list(buttons, stacked, cx))
                .child(div().flex().gap_2().child(render_button(
                    "Reset to defaults",
                    false,
                    |_cx| {
                        backend::push_ui_action(backend::UiAction::ConfirmResetModButtons);
                    },
                    cx,
                ))),
        ))
}

fn render_editor<T: 'static>(
    editor: ModButtonEditorState,
    editor_generation: u64,
    editor_needs_review: bool,
    fields: ModButtonTextFields<T>,
    stacked: bool,
    cx: &mut Context<T>,
) -> impl IntoElement + use<T> {
    let editing = editor.editing_index.is_some();

    div()
        .flex()
        .flex_col()
        .gap_2()
        .child(
            div()
                .text_sm()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(colors::text_primary())
                .child(if editing { "Edit button" } else { "Add button" }),
        )
        .child(setting_target(MOD_BUTTON_LABEL, fields.label.render(cx)))
        .child(render_command_with_variables(fields.command, stacked, cx))
        .child(render_editor_footer(
            div().flex().gap_2().items_center().child(setting_target(
                MOD_BUTTON_DESTRUCTIVE,
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(labelled_toggle(
                        "mod-button-destructive-toggle",
                        "Destructive moderation button",
                        editor.is_destructive,
                        cx,
                        |enabled, cx| {
                            update_editor(|editor| {
                                editor.is_destructive = enabled;
                                editor.status = None;
                            });
                            cx.notify();
                        },
                    ))
                    .child(
                        div()
                            .text_xs()
                            .text_color(colors::text_secondary())
                            .child("Destructive"),
                    ),
            )),
            editing,
            editor_needs_review,
            editor.status,
            move |cx| {
                save_editor_button(editor_generation);
                cx.notify();
            },
            reset_editor,
            cx,
        ))
}

fn render_command_with_variables<T: 'static>(
    command_input: DraftField<T>,
    stacked: bool,
    cx: &mut Context<T>,
) -> impl IntoElement + use<T> {
    div()
        .w_full()
        .min_w_0()
        .flex()
        .gap_3()
        .map(|row| {
            if stacked {
                row.flex_col()
            } else {
                row.items_start()
            }
        })
        .child(
            div()
                .min_w_0()
                .flex_1()
                .child(setting_target(MOD_BUTTON_COMMAND, command_input.render(cx))),
        )
        .child(setting_target(
            MOD_VARIABLES,
            div()
                .min_w_0()
                .map(|reference| {
                    if stacked {
                        reference.w_full()
                    } else {
                        reference.w(px(240.0)).flex_shrink_0()
                    }
                })
                .flex()
                .flex_col()
                .gap_1()
                .child(settings_subheading("Message variables"))
                .child(render_variable_row("{user.name}", "Target login"))
                .child(render_variable_row("{channel.name}", "Channel"))
                .child(render_variable_row("{msg.id}", "Message ID"))
                .child(render_variable_row("{msg.text}", "Message text")),
        ))
}

fn render_button_list<T: 'static>(
    buttons: Vec<ModButtonConfig>,
    stacked: bool,
    cx: &mut Context<T>,
) -> impl IntoElement + use<T> {
    div()
        .flex()
        .flex_col()
        .gap_2()
        .when(buttons.is_empty(), |el| {
            el.child(empty_state("No moderation buttons configured."))
        })
        .children(
            buttons
                .into_iter()
                .enumerate()
                .map(|(index, button)| render_button_row(index, button, stacked, cx)),
        )
}

fn render_button_row<T: 'static>(
    index: usize,
    button: ModButtonConfig,
    stacked: bool,
    cx: &mut Context<T>,
) -> impl IntoElement + use<T> {
    let edit_button = button.clone();

    div()
        .w_full()
        .px_3()
        .py_2()
        .rounded(Radii::sm())
        .border_1()
        .border_color(colors::border())
        .flex()
        .gap_2()
        .map(|row| {
            if stacked {
                row.flex_col().items_stretch()
            } else {
                row.items_center()
            }
        })
        .child(
            div()
                .min_w(px(42.0))
                .px_2()
                .h(px(24.0))
                .rounded(Radii::sm())
                .bg(if button.is_destructive {
                    colors::signal_red().opacity(0.16)
                } else {
                    colors::warning().opacity(0.16)
                })
                .border_1()
                .border_color(if button.is_destructive {
                    colors::signal_red().opacity(0.3)
                } else {
                    colors::warning().opacity(0.3)
                })
                .flex()
                .items_center()
                .justify_center()
                .child(
                    div()
                        .text_xs()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(if button.is_destructive {
                            colors::signal_red()
                        } else {
                            colors::warning()
                        })
                        .child(button.label.clone()),
                ),
        )
        .child(
            div()
                .map(|text| {
                    if stacked {
                        text.w_full().flex_none()
                    } else {
                        text.flex_1().flex_basis(px(260.0))
                    }
                })
                .min_w_0()
                .flex()
                .flex_col()
                .child(
                    div()
                        .text_sm()
                        .font_family("monospace")
                        .whitespace_normal()
                        .text_color(colors::text_primary())
                        .child(button.command.clone()),
                ),
        )
        .child(
            div()
                .flex()
                .gap_2()
                .map(|actions| {
                    if stacked {
                        actions.w_full().justify_end()
                    } else {
                        actions.flex_shrink_0()
                    }
                })
                .child(quiet_action_button(
                    format!("edit-mod-button-{index}"),
                    "Edit",
                    false,
                    cx,
                    move |cx| {
                        set_editor(index, edit_button.clone());
                        cx.notify();
                    },
                ))
                .child(quiet_action_button(
                    format!("delete-mod-button-{index}"),
                    "Delete",
                    true,
                    cx,
                    move |cx| {
                        backend::settings::remove_mod_button(index);
                        if editor_snapshot()
                            .draft
                            .editing_index
                            .is_some_and(|editing_index| index <= editing_index)
                        {
                            reset_editor();
                        }
                        cx.notify();
                    },
                )),
        )
}

fn mod_button_input<T: 'static>(
    key: &'static str,
    field: ModButtonEditorField,
    value: &str,
    placeholder: &'static str,
) -> DraftInput<T> {
    DraftInput::new(key, value, placeholder)
        .on_focus(move |_cx| update_editor(|editor| editor.focused = Some(field)))
        .on_blur(move |_cx| {
            update_editor(|editor| {
                if editor.focused == Some(field) {
                    editor.focused = None;
                }
            });
        })
        .on_change(move |text, _cx| {
            update_editor(|editor| {
                match field {
                    ModButtonEditorField::Label => editor.label = text,
                    ModButtonEditorField::Command => editor.command = text,
                }
                editor.status = None;
            });
        })
        .on_enter(|_cx| save_editor_button(editor_snapshot().generation))
        .on_escape(|_window, _cx| update_editor(|editor| editor.focused = None))
}

fn render_variable_row(name: &'static str, description: &'static str) -> impl IntoElement {
    div()
        .flex()
        .flex_wrap()
        .items_baseline()
        .gap_2()
        .child(
            div()
                .flex_shrink_0()
                .font_family("monospace")
                .text_xs()
                .text_color(colors::text_primary())
                .child(name),
        )
        .child(
            div()
                .min_w_0()
                .text_xs()
                .text_color(colors::text_secondary())
                .whitespace_normal()
                .child(description),
        )
}

fn save_editor_button(expected_generation: u64) {
    let Some(editor) = prepare_editor(expected_generation) else {
        return;
    };
    let label = editor.label.trim().to_string();
    let command = editor.command.trim().to_string();

    if label.is_empty() || command.is_empty() {
        update_editor(|editor| editor.status = Some("Label and command are required".to_string()));
        return;
    }

    let button = ModButtonConfig {
        label,
        command,
        icon: editor.icon.clone(),
        is_destructive: editor.is_destructive,
    };

    let status = if let Some(index) = editor.editing_index {
        if backend::settings::get_mod_buttons().get(index).is_none() {
            MOD_BUTTON_EDITOR.replace_from_authority(ModButtonEditorState {
                status: Some("Moderation button no longer exists; the draft was reset".to_string()),
                ..ModButtonEditorState::default()
            });
            return;
        }
        backend::settings::update_mod_button(index, button);
        "Updated moderation button"
    } else {
        backend::settings::add_mod_button(button);
        "Added moderation button"
    };

    set_editor_state(ModButtonEditorState {
        status: Some(status.to_string()),
        focused: editor.focused,
        ..ModButtonEditorState::default()
    });
}

fn set_editor(index: usize, button: ModButtonConfig) {
    MOD_BUTTON_EDITOR.replace_from_authority(ModButtonEditorState {
        label: button.label,
        command: button.command,
        icon: button.icon,
        editing_index: Some(index),
        is_destructive: button.is_destructive,
        status: None,
        focused: Some(ModButtonEditorField::Label),
    });
}

/// Replace the moderation buttons with the defaults and drop any draft. Runs
/// once the user confirms `UiAction::ConfirmResetModButtons`.
pub(crate) fn reset_mod_buttons_to_defaults() {
    backend::settings::reset_mod_buttons_to_defaults();
    reset_editor();
}

fn reset_editor() {
    set_editor_state(ModButtonEditorState::default());
}

pub(super) fn reset_moderation_editor_after_settings_replace() {
    set_editor_state(ModButtonEditorState {
        status: Some("Moderation-button draft reset after settings were replaced".to_string()),
        ..ModButtonEditorState::default()
    });
}

fn recovered_editor(_previous: &ModButtonEditorState) -> ModButtonEditorState {
    ModButtonEditorState {
        status: Some(
            "Moderation-button draft was reset after an internal editor error. Add or edit a button again before saving."
                .to_string(),
        ),
        ..ModButtonEditorState::default()
    }
}

fn editor_snapshot() -> EditorSnapshot<ModButtonEditorState> {
    MOD_BUTTON_EDITOR.snapshot_or_reset(recovered_editor)
}

fn prepare_editor(expected_generation: u64) -> Option<ModButtonEditorState> {
    MOD_BUTTON_EDITOR.prepare_ready(expected_generation, recovered_editor)
}

fn set_editor_state(state: ModButtonEditorState) {
    MOD_BUTTON_EDITOR.replace_from_authority(state);
}

fn update_editor(update: impl FnOnce(&mut ModButtonEditorState)) {
    let _ = MOD_BUTTON_EDITOR.apply_user_edit(recovered_editor, update);
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test;

    #[test]
    fn poison_recovery_never_reloads_a_positional_edit() {
        let previous = ModButtonEditorState {
            label: "partial".to_string(),
            command: "/ban partial".to_string(),
            icon: Some("shield".to_string()),
            editing_index: Some(3),
            is_destructive: true,
            status: None,
            focused: Some(ModButtonEditorField::Command),
        };

        let recovered = recovered_editor(&previous);
        assert!(recovered.label.is_empty());
        assert!(recovered.command.is_empty());
        assert!(recovered.icon.is_none());
        assert_eq!(recovered.editing_index, None);
        assert!(!recovered.is_destructive);
        assert!(recovered.focused.is_none());
        assert!(recovered.status.is_some());
    }

    #[test]
    fn normal_edit_state_preserves_the_existing_icon() {
        let button = ModButtonConfig {
            label: "Ban".to_string(),
            command: "/ban {user.name}".to_string(),
            icon: Some("shield".to_string()),
            is_destructive: true,
        };
        let editor = ModButtonEditorState {
            label: button.label.clone(),
            command: button.command.clone(),
            icon: button.icon.clone(),
            editing_index: Some(0),
            is_destructive: button.is_destructive,
            status: None,
            focused: Some(ModButtonEditorField::Label),
        };

        assert_eq!(editor.icon, button.icon);
    }

    #[test]
    fn button_summary_prioritizes_review_over_an_empty_draft() {
        let editor = EditorSnapshot {
            draft: ModButtonEditorState::default(),
            generation: 0,
            needs_review: true,
        };

        assert_eq!(
            moderation_buttons_summary(0, &editor),
            "0 buttons · Needs review"
        );
    }
}
