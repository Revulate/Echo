//! Commands settings section.

use super::render_editor_footer;
use crate::backend::settings::{self, CustomCommand};
use crate::components::settings::editor_state::{EditorSnapshot, EditorState};
use crate::components::settings::settings_navigation::{
    SettingDescriptor, SettingsSectionId, SettingsSubsectionId,
};
use crate::components::settings::settings_page::SettingsPage;
use crate::components::settings::widgets::{
    SettingsPageUi, labelled_toggle, quiet_action_button, setting_reset, setting_target,
    settings_defaults, settings_group, settings_subtopic, settings_summary_disclosure,
};
use crate::components::ui::input::{
    Escape, Input, InputEvent, InputState, normalize_single_line_text,
};
use crate::theme::colors;
use crate::theme::tokens::Spacing;
use gpui::prelude::FluentBuilder;
use gpui::*;
use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::LazyLock;
use uuid::Uuid;

const COMMANDS_EDITOR_DISCLOSURE: &str = "commands-editor";

const ENABLE_SLASH_COMMANDS: SettingDescriptor = SettingDescriptor {
    id: "commands.enable-slash",
    label: "Enable slash commands",
    aliases: &["slash", "/command", "local command"],
    control_id: "commands-enable-slash",
    disclosures: &[],
};

const COMMAND_AUTOCOMPLETE: SettingDescriptor = SettingDescriptor {
    id: "commands.autocomplete",
    label: "Command autocomplete",
    aliases: &["slash command completion"],
    control_id: "commands-autocomplete",
    disclosures: &[],
};

const COMMAND_DESCRIPTIONS: SettingDescriptor = SettingDescriptor {
    id: "commands.descriptions",
    label: "Show command descriptions",
    aliases: &["slash command helper text"],
    control_id: "commands-descriptions",
    disclosures: &[],
};

/// Commands › Slash commands.
pub const COMMANDS_SLASH_SETTINGS: &[SettingDescriptor] = &[
    ENABLE_SLASH_COMMANDS,
    COMMAND_AUTOCOMPLETE,
    COMMAND_DESCRIPTIONS,
];

const CUSTOM_COMMAND_TRIGGER: SettingDescriptor = SettingDescriptor {
    id: "commands.custom-trigger",
    label: "Custom command trigger",
    aliases: &["custom command alias"],
    control_id: "commands-trigger-field",
    disclosures: &[COMMANDS_EDITOR_DISCLOSURE],
};

const CUSTOM_COMMAND_RESPONSE: SettingDescriptor = SettingDescriptor {
    id: "commands.custom-response",
    label: "Custom command response",
    aliases: &["custom command expansion"],
    control_id: "commands-response-field",
    disclosures: &[COMMANDS_EDITOR_DISCLOSURE],
};

const CUSTOM_COMMAND_IMPORT_EXPORT: SettingDescriptor = SettingDescriptor {
    id: "commands.import-export",
    label: "Import and export custom commands",
    aliases: &[
        "Import / export custom commands",
        "custom command JSON import",
        "custom command backup export",
    ],
    control_id: "commands-json-field",
    disclosures: &[COMMANDS_EDITOR_DISCLOSURE],
};

/// Commands › Custom commands.
pub const COMMANDS_CUSTOM_SETTINGS: &[SettingDescriptor] = &[
    CUSTOM_COMMAND_TRIGGER,
    CUSTOM_COMMAND_RESPONSE,
    CUSTOM_COMMAND_IMPORT_EXPORT,
];

#[derive(Clone, Copy, PartialEq, Eq)]
enum CommandEditorField {
    Trigger,
    Response,
    ImportExportJson,
}

impl CommandEditorField {
    const ALL: [Self; 3] = [Self::Trigger, Self::Response, Self::ImportExportJson];

    fn index(self) -> usize {
        match self {
            Self::Trigger => 0,
            Self::Response => 1,
            Self::ImportExportJson => 2,
        }
    }

    fn descriptor(self) -> SettingDescriptor {
        match self {
            Self::Trigger => CUSTOM_COMMAND_TRIGGER,
            Self::Response => CUSTOM_COMMAND_RESPONSE,
            Self::ImportExportJson => CUSTOM_COMMAND_IMPORT_EXPORT,
        }
    }

    fn placeholder(self) -> &'static str {
        match self {
            Self::Trigger => "Trigger, for example /hello",
            Self::Response => "Response, for example Hello {1+}",
            Self::ImportExportJson => "Paste custom command JSON",
        }
    }
}

#[derive(Clone, Default)]
struct CommandEditorState {
    trigger: String,
    response: String,
    import_export_json: String,
    import_export_json_dirty: bool,
    editing_id: Option<String>,
    status: Option<String>,
    import_export_status: Option<String>,
    focused: Option<CommandEditorField>,
}

impl CommandEditorState {
    fn value(&self, field: CommandEditorField) -> &str {
        match field {
            CommandEditorField::Trigger => &self.trigger,
            CommandEditorField::Response => &self.response,
            CommandEditorField::ImportExportJson => &self.import_export_json,
        }
    }

    fn value_mut(&mut self, field: CommandEditorField) -> &mut String {
        match field {
            CommandEditorField::Trigger => &mut self.trigger,
            CommandEditorField::Response => &mut self.response,
            CommandEditorField::ImportExportJson => &mut self.import_export_json,
        }
    }
}

static COMMAND_EDITOR: LazyLock<EditorState<CommandEditorState>> =
    LazyLock::new(|| EditorState::new("custom commands", CommandEditorState::default()));

struct CommandEditorInputs {
    fields: [Entity<InputState>; 3],
    _subscriptions: [Subscription; 3],
}

thread_local! {
    static COMMAND_EDITOR_INPUTS: RefCell<HashMap<EntityId, CommandEditorInputs>> = RefCell::new(HashMap::new());
}

/// Drop the retained input entities for a settings host as it closes or leaves
/// Commands. The `EditorState` remains the authority for a recoverable draft.
pub(crate) fn release_command_editor_inputs_for_host(owner: EntityId) {
    let removed = COMMAND_EDITOR_INPUTS
        .try_with(|inputs| inputs.borrow_mut().remove(&owner))
        .ok()
        .flatten();
    drop(removed);
}

/// Render the Commands page as its two scroll anchors.
pub fn render_commands_section<T: 'static>(
    ui: &mut SettingsPageUi,
    window: &mut Window,
    cx: &mut Context<T>,
) -> SettingsPage {
    let slash_commands_enabled = settings::get_slash_commands_enabled();
    let command_autocomplete_enabled = settings::get_command_autocomplete_enabled();
    let show_command_descriptions = settings::get_show_command_descriptions();
    let commands = settings::get_custom_commands();
    let editor = command_editor_snapshot();
    let commands_disclosure = ui.disclosure(COMMANDS_EDITOR_DISCLOSURE, cx);

    if commands_disclosure.is_expanded() {
        ensure_command_editor_inputs(&editor.draft, window, cx);
    } else {
        release_command_editor_inputs_for_host(cx.entity_id());
        if editor.draft.focused.is_some() {
            update_editor(|editor| editor.focused = None);
        }
    }

    SettingsPage::new(SettingsSectionId::Commands)
        .subsection(
            SettingsSubsectionId::SlashCommands,
            render_slash_commands_settings(
                slash_commands_enabled,
                command_autocomplete_enabled,
                show_command_descriptions,
                ui.stacked,
                cx,
            ),
        )
        .subsection(
            SettingsSubsectionId::CustomCommands,
            render_custom_commands_settings(commands, editor, &commands_disclosure, ui.stacked, cx),
        )
}

fn render_slash_commands_settings<T: 'static>(
    slash_commands_enabled: bool,
    command_autocomplete_enabled: bool,
    show_command_descriptions: bool,
    stacked: bool,
    cx: &mut Context<T>,
) -> impl IntoElement + use<T> {
    let defaults = settings_defaults();
    let slash_modified = slash_commands_enabled != defaults.slash_commands_enabled;
    let autocomplete_modified =
        command_autocomplete_enabled != defaults.command_autocomplete_enabled;
    let descriptions_modified = show_command_descriptions != defaults.show_command_descriptions;
    settings_group(
        "commands-slash",
        "Commands",
        true,
        div()
            .flex()
            .flex_col()
            .child(setting_target(
                ENABLE_SLASH_COMMANDS,
                crate::components::settings::widgets::setting_compact_row(
                    ENABLE_SLASH_COMMANDS.id,
                    ENABLE_SLASH_COMMANDS.label,
                    Some("Handle slash commands locally before normal message send."),
                    labelled_toggle(
                        ENABLE_SLASH_COMMANDS.control_id,
                        ENABLE_SLASH_COMMANDS.label,
                        slash_commands_enabled,
                        cx,
                        |enabled, cx| {
                            settings::set_slash_commands_enabled(enabled);
                            cx.notify();
                        },
                    ),
                    slash_modified,
                    slash_modified.then(|| {
                        setting_reset(
                            "reset-commands-enable-slash",
                            ENABLE_SLASH_COMMANDS.label,
                            cx,
                            |cx| {
                                settings::set_slash_commands_enabled(
                                    settings_defaults().slash_commands_enabled,
                                );
                                cx.notify();
                            },
                        )
                    }),
                    stacked,
                ),
            ))
            .child(setting_target(
                COMMAND_AUTOCOMPLETE,
                crate::components::settings::widgets::setting_compact_row(
                    COMMAND_AUTOCOMPLETE.id,
                    COMMAND_AUTOCOMPLETE.label,
                    Some("Show slash command suggestions while typing."),
                    labelled_toggle(
                        COMMAND_AUTOCOMPLETE.control_id,
                        COMMAND_AUTOCOMPLETE.label,
                        command_autocomplete_enabled,
                        cx,
                        |enabled, cx| {
                            settings::set_command_autocomplete_enabled(enabled);
                            cx.notify();
                        },
                    ),
                    autocomplete_modified,
                    autocomplete_modified.then(|| {
                        setting_reset(
                            "reset-commands-autocomplete",
                            COMMAND_AUTOCOMPLETE.label,
                            cx,
                            |cx| {
                                settings::set_command_autocomplete_enabled(
                                    settings_defaults().command_autocomplete_enabled,
                                );
                                cx.notify();
                            },
                        )
                    }),
                    stacked,
                ),
            ))
            .child(setting_target(
                COMMAND_DESCRIPTIONS,
                crate::components::settings::widgets::setting_compact_row(
                    COMMAND_DESCRIPTIONS.id,
                    COMMAND_DESCRIPTIONS.label,
                    Some("Display helper text in command autocomplete."),
                    labelled_toggle(
                        COMMAND_DESCRIPTIONS.control_id,
                        COMMAND_DESCRIPTIONS.label,
                        show_command_descriptions,
                        cx,
                        |enabled, cx| {
                            settings::set_show_command_descriptions(enabled);
                            cx.notify();
                        },
                    ),
                    descriptions_modified,
                    descriptions_modified.then(|| {
                        setting_reset(
                            "reset-commands-descriptions",
                            COMMAND_DESCRIPTIONS.label,
                            cx,
                            |cx| {
                                settings::set_show_command_descriptions(
                                    settings_defaults().show_command_descriptions,
                                );
                                cx.notify();
                            },
                        )
                    }),
                    stacked,
                ),
            )),
    )
}

fn render_custom_commands_settings<T: 'static>(
    commands: Vec<CustomCommand>,
    editor: EditorSnapshot<CommandEditorState>,
    disclosure: &crate::components::settings::widgets::SettingsDisclosureState,
    stacked: bool,
    cx: &mut Context<T>,
) -> impl IntoElement + use<T> {
    let summary = custom_commands_summary(commands.len(), &editor);
    settings_group(
        "commands-custom",
        "Custom commands",
        false,
        settings_summary_disclosure(
            COMMANDS_EDITOR_DISCLOSURE,
            "Custom commands",
            &summary,
            disclosure,
            cx,
            move |cx| render_custom_commands_body(commands, editor, stacked, cx),
        ),
    )
}

fn custom_commands_summary(
    command_count: usize,
    editor: &EditorSnapshot<CommandEditorState>,
) -> String {
    let count = match command_count {
        1 => "1 command".to_string(),
        count => format!("{count} commands"),
    };
    let draft = &editor.draft;
    if editor.needs_review {
        format!("{count} · Needs review")
    } else if draft.editing_id.is_some() {
        format!("{count} · Editing…")
    } else if !draft.trigger.is_empty()
        || !draft.response.is_empty()
        || draft.import_export_json_dirty
    {
        format!("{count} · Unsaved draft")
    } else {
        count
    }
}

fn render_custom_commands_body<T: 'static>(
    commands: Vec<CustomCommand>,
    editor: EditorSnapshot<CommandEditorState>,
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
        .gap(px(Spacing::MD))
        .child(settings_subtopic(
            "commands-command-form",
            if editor.editing_id.is_some() {
                "Edit command"
            } else {
                "Add command"
            },
            div()
                .flex()
                .flex_col()
                .gap(px(Spacing::SM))
                .child(setting_target(
                    CUSTOM_COMMAND_TRIGGER,
                    render_command_input(CommandEditorField::Trigger, cx),
                ))
                .child(setting_target(
                    CUSTOM_COMMAND_RESPONSE,
                    render_command_input(CommandEditorField::Response, cx),
                ))
                .child(render_editor_footer(
                    div().flex().flex_wrap().gap_2().items_center(),
                    editor.editing_id.is_some(),
                    editor_needs_review,
                    editor.status.clone(),
                    move |cx| {
                        save_editor_command(editor_generation);
                        cx.notify();
                    },
                    reset_editor,
                    cx,
                )),
        ))
        .child(settings_subtopic(
            "commands-saved-list",
            "Saved commands",
            render_custom_command_list(commands, stacked, cx),
        ))
        .child(settings_subtopic(
            "commands-import-export",
            "Import and export",
            render_import_export(editor, editor_generation, cx),
        ))
}

fn render_custom_command_list<T: 'static>(
    commands: Vec<CustomCommand>,
    stacked: bool,
    cx: &mut Context<T>,
) -> impl IntoElement + use<T> {
    div()
        .flex()
        .flex_col()
        .gap_0()
        .when(commands.is_empty(), |el| {
            el.child(
                div()
                    .text_sm()
                    .text_color(colors::text_secondary())
                    .child("No custom commands configured."),
            )
        })
        .children(
            commands
                .into_iter()
                .map(|command| render_command_row(command, stacked, cx)),
        )
}

fn render_command_row<T: 'static>(
    command: CustomCommand,
    stacked: bool,
    cx: &mut Context<T>,
) -> impl IntoElement + use<T> {
    let command_for_toggle = command.clone();
    let command_for_edit = command.clone();
    let command_id = command.id.clone();
    let command_id_for_delete = command.id.clone();
    let toggle_label = format!("Enable custom command {}", command.name);

    div()
        .id(SharedString::from(format!("custom-command-{}", command.id)))
        .w_full()
        .min_w_0()
        .min_h(px(40.0))
        .px_2()
        .py_1()
        .border_b_1()
        .border_color(colors::border())
        .flex()
        .flex_wrap()
        .items_center()
        .gap(px(Spacing::SM))
        .map(|row| {
            if stacked {
                row.flex_col().items_stretch()
            } else {
                row
            }
        })
        .child(labelled_toggle(
            format!("command-toggle-{command_id}"),
            &toggle_label,
            command.enabled,
            cx,
            move |enabled, cx| {
                let mut updated = command_for_toggle.clone();
                updated.enabled = enabled;
                settings::update_custom_command(&updated.id.clone(), updated);
                cx.notify();
            },
        ))
        .child(
            div()
                .min_w_0()
                .map(|text| {
                    if stacked {
                        text.w_full().flex_none()
                    } else {
                        text.flex_1().flex_basis(px(220.0))
                    }
                })
                .flex()
                .items_center()
                .gap(px(Spacing::SM))
                .child(
                    div()
                        .text_sm()
                        .flex_shrink_0()
                        .font_family("monospace")
                        .text_color(if command.enabled {
                            colors::text_primary()
                        } else {
                            colors::text_muted()
                        })
                        .child(command.name.clone()),
                )
                .child(
                    div()
                        .min_w_0()
                        .text_xs()
                        .text_color(colors::text_secondary())
                        .overflow_hidden()
                        .text_ellipsis()
                        .whitespace_nowrap()
                        .child(command.response.clone()),
                ),
        )
        .child(
            div()
                .flex()
                .gap(px(Spacing::SM))
                .map(|actions| {
                    if stacked {
                        actions.w_full().justify_end()
                    } else {
                        actions.flex_shrink_0()
                    }
                })
                .child(quiet_action_button(
                    format!("edit-command-{command_id}"),
                    "Edit",
                    false,
                    cx,
                    move |cx| {
                        set_editor_for_command(command_for_edit.clone());
                        cx.notify();
                    },
                ))
                .child(quiet_action_button(
                    format!("delete-command-{command_id_for_delete}"),
                    "Delete",
                    true,
                    cx,
                    move |cx| {
                        settings::remove_custom_command(&command_id_for_delete);
                        if command_editor_snapshot().draft.editing_id.as_deref()
                            == Some(command_id_for_delete.as_str())
                        {
                            reset_editor();
                        }
                        cx.notify();
                    },
                )),
        )
}

fn render_import_export<T: 'static>(
    editor: CommandEditorState,
    editor_generation: u64,
    cx: &mut Context<T>,
) -> impl IntoElement + use<T> {
    div()
        .flex()
        .flex_col()
        .gap(px(Spacing::SM))
        .child(setting_target(
            CUSTOM_COMMAND_IMPORT_EXPORT,
            render_command_input(CommandEditorField::ImportExportJson, cx),
        ))
        .child(
            div()
                .flex()
                .flex_wrap()
                .gap(px(Spacing::SM))
                .child(quiet_action_button(
                    "commands-export-json",
                    "Export JSON",
                    false,
                    cx,
                    |cx| {
                        let status = match settings::export_custom_commands_to_json() {
                            Ok(json) => match compact_json_for_single_line_input(&json) {
                                Ok(compact_json) => {
                                    crate::utils::copy_text(&compact_json);
                                    update_editor(|editor| {
                                        editor.import_export_json = compact_json;
                                        editor.import_export_json_dirty = false;
                                        editor.status = None;
                                        editor.import_export_status =
                                            Some("Exported and copied to clipboard".to_string());
                                    });
                                    None
                                }
                                Err(error) => Some(error),
                            },
                            Err(error) => Some(error),
                        };
                        if let Some(error) = status {
                            update_editor(|editor| {
                                editor.status = None;
                                editor.import_export_status = Some(error);
                            });
                        }
                        cx.notify();
                    },
                ))
                .child(quiet_action_button(
                    "commands-import-json",
                    "Import JSON",
                    false,
                    cx,
                    move |cx| {
                        if let Some(editor) = prepare_command_editor(editor_generation) {
                            let status = match settings::import_custom_commands_from_json(
                                &editor.import_export_json,
                            ) {
                                Ok(count) => (format!("Imported {count} custom commands"), false),
                                Err(error) => (error, true),
                            };
                            update_editor(|editor| {
                                editor.status = None;
                                editor.import_export_status = Some(status.0);
                                if !status.1 {
                                    editor.import_export_json.clear();
                                    editor.import_export_json_dirty = false;
                                }
                            });
                        }
                        cx.notify();
                    },
                ))
                .child(quiet_action_button(
                    "commands-clear-json",
                    "Clear",
                    false,
                    cx,
                    |cx| {
                        update_editor(|editor| {
                            editor.import_export_json.clear();
                            editor.import_export_json_dirty = false;
                            editor.import_export_status = None;
                        });
                        cx.notify();
                    },
                )),
        )
        .when_some(editor.import_export_status, |body, status| {
            body.child(
                div()
                    .text_xs()
                    .text_color(colors::text_secondary())
                    .child(status),
            )
        })
}

fn compact_json_for_single_line_input(json: &str) -> Result<String, String> {
    serde_json::from_str::<serde_json::Value>(json)
        .and_then(|value| serde_json::to_string(&value))
        .map(|json| {
            json.replace('\u{0085}', "\\u0085")
                .replace('\u{2028}', "\\u2028")
                .replace('\u{2029}', "\\u2029")
        })
        .map_err(|error| format!("Export failed: {error}"))
}

fn render_command_input<T: 'static>(
    field: CommandEditorField,
    cx: &mut Context<T>,
) -> impl IntoElement + use<T> {
    let descriptor = field.descriptor();
    let input_for_escape = command_editor_input(field, cx);
    div()
        .w_full()
        .min_w_0()
        .flex()
        .flex_col()
        .gap(px(Spacing::XXS))
        .child(
            div()
                .text_size(px(
                    crate::components::settings::widgets::SettingsMetrics::CALM_LABEL_TEXT,
                ))
                .font_weight(FontWeight::MEDIUM)
                .text_color(colors::text_primary())
                .child(descriptor.label),
        )
        .when_some(input_for_escape, |body, input| {
            let input_for_escape = input.clone();
            body.child(
                div()
                    .id(SharedString::from(descriptor.control_id))
                    .w_full()
                    .min_w_0()
                    .h(px(
                        crate::components::settings::widgets::SettingsMetrics::INPUT_MIN_HEIGHT,
                    ))
                    .font_family("monospace")
                    .capture_action(cx.listener(move |_this, _: &Escape, window, cx| {
                        revert_command_editor_field(field, &input_for_escape, window, cx);
                        cx.stop_propagation();
                        cx.notify();
                    }))
                    .child(Input::new(&input).accessibility_label(descriptor.label)),
            )
        })
}

fn command_editor_input<T: 'static>(
    field: CommandEditorField,
    cx: &Context<T>,
) -> Option<Entity<InputState>> {
    let owner = cx.entity_id();
    COMMAND_EDITOR_INPUTS.with(|inputs| {
        inputs
            .borrow()
            .get(&owner)
            .map(|inputs| inputs.fields[field.index()].clone())
    })
}

fn ensure_command_editor_inputs<T: 'static>(
    editor: &CommandEditorState,
    window: &mut Window,
    cx: &mut Context<T>,
) {
    let owner = cx.entity_id();
    let already_created = COMMAND_EDITOR_INPUTS.with(|inputs| inputs.borrow().contains_key(&owner));
    if already_created {
        sync_command_editor_inputs(editor, window, cx);
        return;
    }

    let fields = CommandEditorField::ALL.map(|field| {
        let value = display_value_for_input(editor, field);
        cx.new(|cx| {
            InputState::new(window, cx)
                .with_placeholder(field.placeholder())
                .with_value(value)
        })
    });
    let subscriptions = CommandEditorField::ALL.map(|field| {
        let input = fields[field.index()].clone();
        cx.subscribe_in(
            &input,
            window,
            move |_this, input, event: &InputEvent, _window, cx| {
                match event {
                    InputEvent::Focus => update_editor(|editor| editor.focused = Some(field)),
                    InputEvent::Change => {
                        let value = input.read(cx).value().to_string();
                        update_editor(|editor| {
                            if display_value_for_input(editor, field) == value {
                                return;
                            }
                            *editor.value_mut(field) = value;
                            if field == CommandEditorField::ImportExportJson {
                                editor.import_export_json_dirty = true;
                                editor.import_export_status = None;
                            } else {
                                editor.status = None;
                            }
                        });
                    }
                    InputEvent::PressEnter => {
                        if field != CommandEditorField::ImportExportJson {
                            let generation = command_editor_snapshot().generation;
                            save_editor_command(generation);
                        }
                    }
                    InputEvent::Blur => update_editor(|editor| {
                        if editor.focused == Some(field) {
                            editor.focused = None;
                        }
                    }),
                }
                cx.notify();
            },
        )
    });
    COMMAND_EDITOR_INPUTS.with(|inputs| {
        inputs.borrow_mut().insert(
            owner,
            CommandEditorInputs {
                fields,
                _subscriptions: subscriptions,
            },
        );
    });
}

fn sync_command_editor_inputs<T: 'static>(
    editor: &CommandEditorState,
    window: &mut Window,
    cx: &mut Context<T>,
) {
    for field in CommandEditorField::ALL {
        let Some(input) = command_editor_input(field, cx) else {
            continue;
        };
        let value = display_value_for_input(editor, field);
        if input.read(cx).value() == value {
            continue;
        }
        input.update(cx, |input, cx| {
            input.set_value(value, window, cx);
        });
    }
}

fn display_value_for_input(editor: &CommandEditorState, field: CommandEditorField) -> String {
    normalize_single_line_text(editor.value(field))
}

fn editor_field_authority(field: CommandEditorField, editor: &CommandEditorState) -> String {
    let Some(editing_id) = editor.editing_id.as_deref() else {
        return String::new();
    };
    let Some(command) = settings::get_custom_commands()
        .into_iter()
        .find(|command| command.id == editing_id)
    else {
        return String::new();
    };
    match field {
        CommandEditorField::Trigger => command.name,
        CommandEditorField::Response => command.response,
        CommandEditorField::ImportExportJson => String::new(),
    }
}

fn revert_command_editor_field<T: 'static>(
    field: CommandEditorField,
    input: &Entity<InputState>,
    window: &mut Window,
    cx: &mut Context<T>,
) {
    let snapshot = command_editor_snapshot();
    let authority = editor_field_authority(field, &snapshot.draft);
    update_editor(|editor| {
        *editor.value_mut(field) = authority.clone();
        if field == CommandEditorField::ImportExportJson {
            editor.import_export_json_dirty = false;
            editor.import_export_status = None;
        } else {
            editor.status = None;
        }
    });
    let display_authority = normalize_single_line_text(&authority);
    if input.read(cx).value() != display_authority {
        input.update(cx, |input, cx| {
            input.set_value(display_authority, window, cx)
        });
    }
}

fn save_editor_command(expected_generation: u64) {
    let Some(editor) = prepare_command_editor(expected_generation) else {
        return;
    };
    let trigger = normalize_single_line_text(&editor.trigger)
        .trim()
        .to_string();
    let response = normalize_single_line_text(&editor.response)
        .trim()
        .to_string();

    if trigger.is_empty() || response.is_empty() {
        update_editor(|editor| {
            editor.status = Some("Trigger and response are required".to_string())
        });
        return;
    }

    let enabled = editor
        .editing_id
        .as_deref()
        .and_then(|id| {
            settings::get_custom_commands()
                .into_iter()
                .find(|command| command.id == id)
                .map(|command| command.enabled)
        })
        .unwrap_or(true);

    let command = CustomCommand {
        id: editor
            .editing_id
            .clone()
            .unwrap_or_else(|| Uuid::new_v4().to_string()),
        name: trigger,
        response,
        enabled,
    };

    let status = if editor.editing_id.is_some() {
        settings::update_custom_command(&command.id.clone(), command);
        "Updated custom command"
    } else {
        settings::add_custom_command(command);
        "Added custom command"
    };

    set_command_editor_state(CommandEditorState {
        import_export_json: editor.import_export_json,
        import_export_json_dirty: editor.import_export_json_dirty,
        status: Some(status.to_string()),
        import_export_status: editor.import_export_status,
        ..CommandEditorState::default()
    });
}

fn set_editor_for_command(command: CustomCommand) {
    let previous = command_editor_snapshot().draft;
    set_command_editor_state(CommandEditorState {
        trigger: normalize_single_line_text(&command.name),
        response: normalize_single_line_text(&command.response),
        import_export_json: previous.import_export_json,
        import_export_json_dirty: previous.import_export_json_dirty,
        editing_id: Some(command.id),
        status: None,
        import_export_status: None,
        ..CommandEditorState::default()
    });
}

fn reset_editor() {
    set_command_editor_state(CommandEditorState::default());
}

pub(super) fn reset_command_editor_after_settings_replace() {
    set_command_editor_state(CommandEditorState {
        status: Some("Command draft reset after settings were replaced".to_string()),
        ..CommandEditorState::default()
    });
}

fn recovered_command_editor_from(
    previous: &CommandEditorState,
    commands: &[CustomCommand],
) -> CommandEditorState {
    let mut editor = previous
        .editing_id
        .as_deref()
        .and_then(|editing_id| commands.iter().find(|command| command.id == editing_id))
        .map(|command| CommandEditorState {
            trigger: normalize_single_line_text(&command.name),
            response: normalize_single_line_text(&command.response),
            editing_id: Some(command.id.clone()),
            ..CommandEditorState::default()
        })
        .unwrap_or_default();
    editor.status = Some(
        "Command draft was reset after an internal editor error. Review or edit it before saving."
            .to_string(),
    );
    editor
}

fn recovered_command_editor(previous: &CommandEditorState) -> CommandEditorState {
    let commands = settings::get_custom_commands();
    recovered_command_editor_from(previous, &commands)
}

fn command_editor_snapshot() -> EditorSnapshot<CommandEditorState> {
    COMMAND_EDITOR.snapshot_or_reset(recovered_command_editor)
}

fn prepare_command_editor(expected_generation: u64) -> Option<CommandEditorState> {
    COMMAND_EDITOR.prepare_ready(expected_generation, recovered_command_editor)
}

fn set_command_editor_state(state: CommandEditorState) {
    COMMAND_EDITOR.replace_from_authority(state);
}

fn update_editor(update: impl FnOnce(&mut CommandEditorState)) {
    let _ = COMMAND_EDITOR.apply_user_edit(recovered_command_editor, update);
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test;

    fn command(id: &str, name: &str, response: &str) -> CustomCommand {
        CustomCommand {
            id: id.to_string(),
            name: name.to_string(),
            response: response.to_string(),
            enabled: true,
        }
    }

    #[test]
    fn recovery_reloads_the_exact_persisted_command_by_id() {
        let previous = CommandEditorState {
            trigger: "partial".to_string(),
            response: "partial".to_string(),
            import_export_json: "unsaved".to_string(),
            import_export_json_dirty: true,
            editing_id: Some("two".to_string()),
            status: None,
            import_export_status: None,
            focused: Some(CommandEditorField::Response),
        };
        let commands = [
            command("one", "/one", "first"),
            command("two", "/two", "second"),
        ];

        let recovered = recovered_command_editor_from(&previous, &commands);
        assert_eq!(recovered.editing_id.as_deref(), Some("two"));
        assert_eq!(recovered.trigger, "/two");
        assert_eq!(recovered.response, "second");
        assert!(recovered.import_export_json.is_empty());
        assert!(!recovered.import_export_json_dirty);
        assert!(recovered.focused.is_none());
        assert!(recovered.status.is_some());
    }

    #[test]
    fn recovery_uses_a_marked_new_form_when_the_command_was_removed() {
        let previous = CommandEditorState {
            editing_id: Some("removed".to_string()),
            trigger: "partial".to_string(),
            ..CommandEditorState::default()
        };

        let recovered = recovered_command_editor_from(&previous, &[]);
        assert_eq!(recovered.editing_id, None);
        assert!(recovered.trigger.is_empty());
        assert!(recovered.response.is_empty());
        assert!(recovered.status.is_some());
    }

    #[test]
    fn summary_ignores_exported_json_but_marks_typed_json() {
        let mut editor = EditorSnapshot {
            generation: 0,
            draft: CommandEditorState {
                import_export_json: "exported".to_string(),
                import_export_json_dirty: false,
                ..CommandEditorState::default()
            },
            needs_review: false,
        };
        assert_eq!(custom_commands_summary(0, &editor), "0 commands");
        editor.draft.import_export_json_dirty = true;
        assert_eq!(
            custom_commands_summary(0, &editor),
            "0 commands · Unsaved draft"
        );
    }

    #[test]
    fn summary_prioritizes_recovery_then_editing() {
        let mut editor = EditorSnapshot {
            generation: 0,
            draft: CommandEditorState {
                editing_id: Some("existing".to_string()),
                ..CommandEditorState::default()
            },
            needs_review: false,
        };
        assert_eq!(custom_commands_summary(1, &editor), "1 command · Editing…");
        editor.needs_review = true;
        assert_eq!(
            custom_commands_summary(1, &editor),
            "1 command · Needs review"
        );
    }

    #[test]
    fn export_json_is_compact_for_the_single_line_input() {
        let compact =
            compact_json_for_single_line_input("[\n  {\n    \"name\": \"/hello\"\n  }\n]")
                .expect("valid command JSON");
        assert_eq!(compact, r#"[{"name":"/hello"}]"#);
        assert!(
            !compact
                .chars()
                .any(|character| matches!(character, '\r' | '\n' | '\u{2028}' | '\u{2029}'))
        );
    }

    #[test]
    fn export_escapes_unicode_line_separators_without_changing_json_values() {
        let source = serde_json::json!({"response": "one\u{0085}two\u{2028}three\u{2029}four"});
        let compact = compact_json_for_single_line_input(&source.to_string()).unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&compact).unwrap(),
            source
        );
        assert_eq!(normalize_single_line_text(&compact), compact);
    }

    #[test]
    fn editing_imported_multiline_commands_flattens_the_response_into_one_message() {
        let saved = command("fixture", "/hello", "first\r\nsecond\u{2028}third");
        let previous = CommandEditorState {
            editing_id: Some(saved.id.clone()),
            ..CommandEditorState::default()
        };
        let editor = recovered_command_editor_from(&previous, &[saved.clone()]);
        assert_eq!(editor.response, "first second third");
        assert_eq!(
            display_value_for_input(&editor, CommandEditorField::Response),
            editor.response
        );
        assert_eq!(saved.response, "first\r\nsecond\u{2028}third");
    }
}
