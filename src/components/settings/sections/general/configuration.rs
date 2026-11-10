//! Advanced › Configuration: settings file actions and the import panel.

use super::*;

pub(super) fn render_configuration_actions<T: 'static>(
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
    let chatterino_import = super::super::chatterino_import::render_chatterino_import(ui, cx);
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
                                    super::super::reset_migrated_editor_drafts_after_settings_replace();
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

pub(super) fn settings_import_editor_snapshot() -> EditorSnapshot<SettingsImportEditor> {
    SETTINGS_IMPORT_EDITOR.snapshot_or_reset(recovered_settings_import_editor)
}

fn prepare_settings_import(expected_generation: u64) -> Option<SettingsImportEditor> {
    SETTINGS_IMPORT_EDITOR.prepare_ready(expected_generation, recovered_settings_import_editor)
}

pub(super) fn update_settings_import_editor(
    update: impl FnOnce(&mut SettingsImportEditor),
) -> bool {
    matches!(
        SETTINGS_IMPORT_EDITOR.apply_user_edit(recovered_settings_import_editor, update),
        crate::components::settings::editor_state::EditorEditResult::Applied
    )
}

pub(crate) fn reset_settings_import_editor_after_settings_replace() {
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
        let editor = player_paths_editor_from_values(
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
