//! Advanced › Configuration › Import from Chatterino.
//!
//! One summary disclosure holds the whole flow: the folder, a preview of
//! every group with its count and a switch, the tab placement choice, what
//! will be skipped and why, and the Import button with its result line.

use crate::backend::bridge::chatterino_import::{
    self as import, ImportGroup, ImportPanelState, ImportPlan, ImportStatus, SourceOrigin,
    TabImportMode,
};
use crate::components::settings::settings_navigation::SettingDescriptor;
use crate::components::settings::widgets::{
    SelectOption, SettingsPageUi, quiet_action_button, select_buttons, setting_compact_row,
    setting_row, setting_target, settings_group, settings_path_row, settings_subtopic,
    settings_summary_disclosure, toggle_with_enabled,
};
use crate::components::ui::ButtonExt;
use crate::theme::colors;
use gpui::prelude::FluentBuilder;
use gpui::*;
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;

const CHATTERINO_IMPORT_DISCLOSURE: &str = "advanced-chatterino-import";

/// Skipped entries listed before the rest are summarized as a count.
const MAX_LISTED_SKIPS: usize = 40;

pub(super) const CHATTERINO_IMPORT: SettingDescriptor = SettingDescriptor {
    id: "developer-tools.chatterino-import",
    label: "Import from Chatterino",
    aliases: &[
        "chatterino",
        "chatterino7",
        "migrate",
        "switch from chatterino",
    ],
    control_id: "advanced-chatterino-import-trigger",
    disclosures: &[],
};

pub(super) const CHATTERINO_IMPORT_FOLDER: SettingDescriptor = SettingDescriptor {
    id: import::SETTINGS_TARGET_ID,
    label: "Chatterino folder",
    aliases: &[
        "chatterino settings folder",
        "portable chatterino",
        "choose folder",
    ],
    control_id: "chatterino-import-choose-folder",
    disclosures: &[CHATTERINO_IMPORT_DISCLOSURE],
};

pub(super) const CHATTERINO_IMPORT_GROUPS: SettingDescriptor = SettingDescriptor {
    id: "developer-tools.chatterino-import-groups",
    label: "What to import from Chatterino",
    aliases: &[
        "chatterino tabs",
        "chatterino highlights",
        "chatterino nicknames",
        "chatterino commands",
        "chatterino filters",
    ],
    control_id: "chatterino-import-group-tabs",
    disclosures: &[CHATTERINO_IMPORT_DISCLOSURE],
};

pub(super) const CHATTERINO_IMPORT_TAB_MODE: SettingDescriptor = SettingDescriptor {
    id: "developer-tools.chatterino-import-tab-mode",
    label: "Imported tabs",
    aliases: &["add to my tabs", "replace my tabs"],
    control_id: "chatterino-import-tab-mode-segmented",
    disclosures: &[CHATTERINO_IMPORT_DISCLOSURE],
};

pub(super) const CHATTERINO_IMPORT_RUN: SettingDescriptor = SettingDescriptor {
    id: "developer-tools.chatterino-import-run",
    label: "Import Chatterino setup",
    aliases: &["import now", "bring over chatterino"],
    control_id: "chatterino-import-run",
    disclosures: &[CHATTERINO_IMPORT_DISCLOSURE],
};

thread_local! {
    /// Per settings host: the disclosure state last rendered and whether it
    /// was expanded, so a fresh expansion can rescan Chatterino's files.
    static LAST_EXPANSION: RefCell<HashMap<EntityId, (usize, bool)>> =
        RefCell::new(HashMap::new());
}

/// Whether the disclosure just went from collapsed (or unseen) to expanded.
fn newly_expanded(host: EntityId, instance: usize, expanded: bool) -> bool {
    LAST_EXPANSION.with(|last| {
        let previous = last.borrow_mut().insert(host, (instance, expanded));
        expanded && previous != Some((instance, true))
    })
}

pub(super) fn render_chatterino_import<T: 'static>(
    ui: &mut SettingsPageUi,
    cx: &mut Context<T>,
) -> impl IntoElement + use<T> {
    let disclosure = ui.disclosure(CHATTERINO_IMPORT_DISCLOSURE, cx);
    let state = import::panel_snapshot();
    let fresh = newly_expanded(
        cx.entity_id(),
        disclosure.instance_key(),
        disclosure.is_expanded(),
    );
    if disclosure.is_expanded() && !state.busy && (fresh || (!state.scanned && !state.scanning)) {
        start_scan(state.rescan_dir(), false, cx);
    }
    let state = import::panel_snapshot();
    let summary = disclosure_summary(&state);
    let stacked = ui.stacked;
    settings_group(
        "advanced-chatterino-import-group",
        CHATTERINO_IMPORT.label,
        false,
        setting_target(
            CHATTERINO_IMPORT,
            settings_summary_disclosure(
                CHATTERINO_IMPORT_DISCLOSURE,
                CHATTERINO_IMPORT.label,
                &summary,
                &disclosure,
                cx,
                move |cx| render_import_panel(state, stacked, cx),
            ),
        ),
    )
}

fn disclosure_summary(state: &ImportPanelState) -> String {
    if state.busy {
        return "Importing…".to_string();
    }
    if let Some(status) = &state.status {
        return if status.is_error {
            "Import didn't run; open for details".to_string()
        } else {
            "Import finished; open for details".to_string()
        };
    }
    if state.scanning {
        return "Looking for Chatterino…".to_string();
    }
    match (&state.plan, state.scanned) {
        (Some(plan), _) if !plan.is_empty() => {
            let found: Vec<String> = [ImportGroup::Tabs, ImportGroup::Highlights]
                .into_iter()
                .filter(|group| plan.count(*group) > 0)
                .map(|group| group.count_phrase(plan.count(group)))
                .collect();
            if found.is_empty() {
                "Chatterino setup found".to_string()
            } else {
                format!("Found {}", found.join(" and "))
            }
        }
        (_, true) => "No Chatterino setup found".to_string(),
        _ => "Bring over tabs, highlights and more".to_string(),
    }
}

/// Read a folder off the UI thread. `chosen` replaces auto-detection; a scan
/// the user asked for clears the previous result line.
fn start_scan<T: 'static>(chosen: Option<PathBuf>, clear_status: bool, cx: &mut Context<T>) {
    let generation = import::begin_scan(clear_status);
    cx.spawn(async move |this, cx| {
        let result = cx
            .background_executor()
            .spawn(async move { import::run_scan(chosen) })
            .await;
        import::finish_scan(generation, result);
        let _ = this.update(cx, |_this, cx| cx.notify());
    })
    .detach();
}

fn choose_folder<T: 'static>(cx: &mut Context<T>) {
    let picked = cx.prompt_for_paths(PathPromptOptions {
        files: false,
        directories: true,
        multiple: false,
        prompt: Some("Choose your Chatterino folder".into()),
    });
    cx.spawn(async move |this, cx| {
        let path = match picked.await {
            Ok(Ok(Some(paths))) => paths.into_iter().next(),
            Ok(Ok(None)) | Err(_) => None,
            Ok(Err(error)) => {
                import::set_status(Some(ImportStatus {
                    message: format!("Couldn't open a folder picker ({error})"),
                    is_error: true,
                }));
                None
            }
        };
        let _ = this.update(cx, |_this, cx| {
            if let Some(path) = path {
                start_scan(Some(path), true, cx);
            }
            cx.notify();
        });
    })
    .detach();
}

fn quiet_line(text: impl Into<SharedString>) -> Div {
    div()
        .w_full()
        .min_w_0()
        .text_xs()
        .text_color(colors::text_muted())
        .child(text.into())
}

fn render_import_panel<T: 'static>(
    state: ImportPanelState,
    stacked: bool,
    cx: &mut Context<T>,
) -> impl IntoElement + use<T> {
    let folder = match &state.source {
        Some((dir, _)) => dir.display().to_string(),
        None if state.scanning || !state.scanned => "Looking for Chatterino…".to_string(),
        None => "No Chatterino setup found".to_string(),
    };
    let folder_label = match state.source.as_ref().map(|(_, origin)| *origin) {
        Some(SourceOrigin::Override) => "Chatterino folder (ECHO_CHATTERINO_IMPORT_DIR)",
        _ => CHATTERINO_IMPORT_FOLDER.label,
    };
    // Scanning again keeps a folder the user picked by hand.
    let rescan_dir = state.rescan_dir();
    let folder_actions = div()
        .flex()
        .flex_wrap()
        .gap_2()
        .child(
            quiet_action_button(
                CHATTERINO_IMPORT_FOLDER.control_id,
                "Choose folder…",
                false,
                cx,
                choose_folder,
            )
            .aria_label("Choose the Chatterino folder to import from"),
        )
        .child(
            quiet_action_button(
                "chatterino-import-rescan",
                "Scan again",
                false,
                cx,
                move |cx| {
                    start_scan(rescan_dir.clone(), true, cx);
                    cx.notify();
                },
            )
            .aria_label("Look for a Chatterino setup again"),
        );

    div()
        .w_full()
        .min_w_0()
        .flex()
        .flex_col()
        .gap_2()
        .child(setting_target(
            CHATTERINO_IMPORT_FOLDER,
            settings_path_row(
                CHATTERINO_IMPORT_FOLDER.id,
                folder_label,
                &folder,
                folder_actions,
                stacked,
            ),
        ))
        .child(quiet_line(import::NOT_IMPORTED_NOTE))
        .when_some(state.plan.clone(), |panel, plan| {
            panel.child(render_preview(&state, &plan, stacked, cx))
        })
        .when(state.plan.is_none(), |panel| {
            panel.children(render_status(&state))
        })
}

fn group_help(plan: &ImportPlan, group: ImportGroup) -> String {
    let count = plan.count(group);
    let mut help = if count == 0 {
        "Nothing found".to_string()
    } else if group == ImportGroup::Tabs {
        let splits = plan.split_count();
        format!(
            "{}, {splits} {}",
            group.count_phrase(count),
            if splits == 1 { "split" } else { "splits" }
        )
    } else {
        group.count_phrase(count)
    };
    let skipped = plan.skipped_in(group).count();
    if skipped > 0 {
        help.push_str(&format!(", {skipped} skipped"));
    }
    help.push('.');
    if count > 0 && !group.checked_by_default() {
        help.push_str(" Off by default: review these before Echo runs them.");
    }
    for note in plan.notes_for(group) {
        help.push(' ');
        help.push_str(&note.text);
    }
    help
}

/// The last result, on its own full-width line so long summaries wrap.
fn render_status(state: &ImportPanelState) -> Option<Stateful<Div>> {
    let status = state.status.clone()?;
    Some(
        div()
            .id("chatterino-import-status")
            .role(Role::Status)
            .aria_label(SharedString::from(status.message.clone()))
            .w_full()
            .min_w_0()
            .text_xs()
            .text_color(if status.is_error {
                colors::signal_red()
            } else {
                colors::text_secondary()
            })
            .child(status.message),
    )
}

fn render_preview<T: 'static>(
    state: &ImportPanelState,
    plan: &ImportPlan,
    stacked: bool,
    cx: &mut Context<T>,
) -> impl IntoElement + use<T> {
    let mut groups = div().w_full().min_w_0().flex().flex_col();
    for group in ImportGroup::ALL {
        let count = plan.count(group);
        let help = group_help(plan, group);
        let selected = state.selection.includes(group) && count > 0;
        let label = group.label();
        let switch = toggle_with_enabled(
            format!("chatterino-import-group-{}", group.slug()),
            selected,
            count > 0,
            cx,
            move |on, cx| {
                import::set_group_selected(group, on);
                cx.notify();
            },
        )
        .aria_label(SharedString::from(format!("Import {label}")));
        groups = groups.child(setting_compact_row(
            format!("chatterino-import-{}-row", group.slug()),
            label,
            Some(&help),
            switch,
            false,
            None,
            stacked,
        ));
    }

    let tab_mode = match state.selection.tab_mode {
        TabImportMode::Append => "append",
        TabImportMode::Replace => "replace",
    };
    let tab_mode_row = (!plan.tabs.is_empty()).then(|| {
        setting_target(
            CHATTERINO_IMPORT_TAB_MODE,
            setting_row(
                CHATTERINO_IMPORT_TAB_MODE.id,
                CHATTERINO_IMPORT_TAB_MODE.label,
                Some(
                    "Replacing asks first. Your tabs are backed up, and Reopen Closed Tab \
                     brings them back until Echo closes.",
                ),
                select_buttons(
                    "chatterino-import-tab-mode",
                    &[
                        SelectOption::new("append", "Add to my tabs"),
                        SelectOption::new("replace", "Replace my tabs"),
                    ],
                    tab_mode,
                    cx,
                    |value, cx| {
                        import::set_tab_mode(if value == "replace" {
                            TabImportMode::Replace
                        } else {
                            TabImportMode::Append
                        });
                        cx.notify();
                    },
                ),
                false,
                None,
                stacked,
            ),
        )
    });

    let skipped: Vec<String> = plan
        .skipped
        .iter()
        .map(|item| match item.group {
            Some(group) => format!("{} · {} — {}", group.label(), item.item, item.reason),
            None => format!("{} — {}", item.item, item.reason),
        })
        .collect();
    let skipped_list = (!skipped.is_empty()).then(|| {
        let hidden = skipped.len().saturating_sub(MAX_LISTED_SKIPS);
        settings_subtopic(
            "chatterino-import-skipped",
            "Not imported",
            div()
                .id("chatterino-import-skipped-list")
                .role(Role::List)
                .aria_label("Chatterino entries that won't be imported")
                .flex()
                .flex_col()
                .gap_1()
                .children(skipped.into_iter().take(MAX_LISTED_SKIPS).enumerate().map(
                    |(index, line)| {
                        div()
                            .id(SharedString::from(format!(
                                "chatterino-import-skipped-{index}"
                            )))
                            .role(Role::ListItem)
                            .text_xs()
                            .text_color(colors::text_secondary())
                            .child(line)
                    },
                ))
                .when(hidden > 0, |list| {
                    list.child(
                        div()
                            .text_xs()
                            .text_color(colors::text_muted())
                            .child(format!("…and {hidden} more")),
                    )
                }),
        )
    });

    let can_import = state.can_import();
    let run_label = if state.busy { "Importing…" } else { "Import" };
    let hint = if state.busy {
        None
    } else if state.scanning {
        Some("Reading Chatterino's files…")
    } else if !state.has_selected_items() {
        Some("Choose at least one group with something to import.")
    } else {
        None
    };
    // A disabled button is built without the interactive hover/active styles:
    // GPUI panics if `disabled_button` re-sets a hover style.
    let run_button = if can_import {
        quiet_action_button(
            CHATTERINO_IMPORT_RUN.control_id,
            run_label,
            false,
            cx,
            |cx| {
                if let Err(message) = import::request_import() {
                    import::set_status(Some(ImportStatus {
                        message,
                        is_error: true,
                    }));
                }
                cx.notify();
            },
        )
        .aria_label("Import the selected Chatterino groups")
    } else {
        disabled_quiet_button(CHATTERINO_IMPORT_RUN.control_id, run_label)
            .aria_label("Import the selected Chatterino groups")
    };

    div()
        .w_full()
        .min_w_0()
        .flex()
        .flex_col()
        .gap_2()
        .child(setting_target(CHATTERINO_IMPORT_GROUPS, groups))
        .children(tab_mode_row)
        .children(skipped_list)
        .child(
            div()
                .w_full()
                .min_w_0()
                .flex()
                .flex_col()
                .items_start()
                .gap_1()
                .child(setting_target(CHATTERINO_IMPORT_RUN, run_button))
                .children(hint.map(quiet_line))
                .children(render_status(state)),
        )
}

/// Same look as `quiet_action_button`, but inert: no hover, focus or click.
fn disabled_quiet_button(id: &'static str, label: &str) -> Stateful<Div> {
    use crate::components::settings::widgets::SettingsMetrics;
    use crate::theme::tokens::{Radii, RadiiPixels, Spacing};
    div()
        .id(id)
        .role(Role::Button)
        .rounded(Radii::md())
        .px(px(Spacing::SM))
        .py(px(Spacing::XS))
        .min_h(px(SettingsMetrics::CONTROL_MIN_HEIGHT))
        .flex_shrink_0()
        .border_1()
        .border_color(colors::border())
        .bg(colors::bg_surface())
        .text_size(px(SettingsMetrics::CALM_HELPER_TEXT))
        .text_color(colors::text_secondary())
        .child(label.to_string())
        .disabled_button()
}
