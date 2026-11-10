//! Nicknames settings section.
//!
//! Local user renames: each rule rewrites a sender's rendered display name at
//! message layout time. Rules target an optional stable Twitch ID first, then
//! match the display name or login exactly or by regex.

use super::render_editor_footer;
use crate::backend::settings::{self, NicknameRule};
use crate::components::settings::draft_input::{DraftField, DraftInput};
use crate::components::settings::editor_state::{EditorSnapshot, EditorState};
use crate::components::settings::settings_navigation::SettingDescriptor;
use crate::components::settings::widgets::{
    SettingsPageUi, action_button_keyed, button as render_button, empty_state, labelled_toggle,
    setting_target, settings_group, settings_group_contents, settings_summary_disclosure,
};
use crate::components::ui::PressToClick;
use crate::theme::colors;
use crate::theme::tokens::{Radii, RadiiPixels};
use gpui::prelude::FluentBuilder;
use gpui::*;
use std::sync::LazyLock;

const NICKNAMES_DISCLOSURE: &str = "chat-nicknames";

const NICKNAME_PATTERN: SettingDescriptor = SettingDescriptor {
    id: "nicknames.pattern",
    label: "Nickname rules",
    aliases: &[
        "nickname rule editor",
        "nickname capture group",
        "stable twitch user id",
    ],
    control_id: "nickname-pattern",
    disclosures: &[NICKNAMES_DISCLOSURE],
};

const NICKNAME_USER_ID: SettingDescriptor = SettingDescriptor {
    id: "nicknames.user-id",
    label: "Nickname Twitch user ID",
    aliases: &["stable user id", "exact target", "account id"],
    control_id: "nicknames-user-id-field",
    disclosures: &[NICKNAMES_DISCLOSURE],
};

const NICKNAME_REPLACEMENT: SettingDescriptor = SettingDescriptor {
    id: "nicknames.replacement",
    label: "Nickname replacement",
    aliases: &["nickname display-name replacement", "local nickname rename"],
    control_id: "nicknames-replacement-field",
    disclosures: &[NICKNAMES_DISCLOSURE],
};

const NICKNAME_REGEX: SettingDescriptor = SettingDescriptor {
    id: "nicknames.regex",
    label: "Nickname rule regex",
    aliases: &["nickname regular expression"],
    control_id: "nickname-flag-regex",
    disclosures: &[NICKNAMES_DISCLOSURE],
};

const NICKNAME_CASE_SENSITIVE: SettingDescriptor = SettingDescriptor {
    id: "nicknames.case-sensitive",
    label: "Nickname rule case sensitive",
    aliases: &["nickname case matching"],
    control_id: "nickname-flag-case-sensitive",
    disclosures: &[NICKNAMES_DISCLOSURE],
};

/// Nicknames (page leaf).
pub const NICKNAMES_SETTINGS: &[SettingDescriptor] = &[
    NICKNAME_PATTERN,
    NICKNAME_USER_ID,
    NICKNAME_REPLACEMENT,
    NICKNAME_REGEX,
    NICKNAME_CASE_SENSITIVE,
];

#[derive(Clone, Copy, PartialEq, Eq)]
enum NicknameEditorField {
    Pattern,
    UserId,
    Replacement,
}

#[derive(Clone, Default)]
struct NicknameEditorState {
    pattern: String,
    user_id: String,
    replacement: String,
    regex: bool,
    case_sensitive: bool,
    editing_id: Option<String>,
    status: Option<String>,
    focused: Option<NicknameEditorField>,
}

static NICKNAME_EDITOR: LazyLock<EditorState<NicknameEditorState>> =
    LazyLock::new(|| EditorState::new("nicknames", NicknameEditorState::default()));

/// A compact Chat summary with the existing full-width nickname editor behind it.
pub fn render_nicknames_section<T: 'static>(
    ui: &mut SettingsPageUi,
    window: &mut Window,
    cx: &mut Context<T>,
) -> impl IntoElement + use<T> {
    let rules = settings::get_nickname_rules();
    let editor = nickname_editor_snapshot_from(&rules);
    let rule_summary = match rules.len() {
        1 => "1 rule".to_string(),
        count => format!("{count} rules"),
    };
    let has_unsaved_draft = !editor.draft.pattern.is_empty()
        || !editor.draft.user_id.is_empty()
        || !editor.draft.replacement.is_empty()
        || editor.draft.regex
        || editor.draft.case_sensitive
        || editor.needs_review;
    let summary = if editor.draft.editing_id.is_some() {
        format!("{rule_summary} · Editing…")
    } else if has_unsaved_draft {
        format!("{rule_summary} · Unsaved draft")
    } else {
        rule_summary
    };
    let disclosure = ui.disclosure(NICKNAMES_DISCLOSURE, cx);
    let fields = NicknameTextFields {
        pattern: nickname_input(
            "nicknames-pattern-field",
            NicknameEditorField::Pattern,
            &editor.draft.pattern,
            "Username to match, for example nightbot",
        )
        .escape_focus(disclosure.clone())
        .prepare(window, cx),
        user_id: nickname_input(
            "nicknames-user-id-field",
            NicknameEditorField::UserId,
            &editor.draft.user_id,
            "Optional Twitch user ID (exact target)",
        )
        .escape_focus(disclosure.clone())
        .prepare(window, cx),
        replacement: nickname_input(
            "nicknames-replacement-field",
            NicknameEditorField::Replacement,
            &editor.draft.replacement,
            "Nickname to show instead",
        )
        .escape_focus(disclosure.clone())
        .prepare(window, cx),
    };

    settings_group(
        "chat-nicknames-options",
        "Nicknames",
        false,
        settings_summary_disclosure(
            NICKNAMES_DISCLOSURE,
            "Nicknames",
            &summary,
            &disclosure,
            cx,
            move |cx| render_nicknames_body(rules, editor, fields, cx),
        ),
    )
}

/// Text inputs for the nickname editor, prepared while the window is available.
struct NicknameTextFields<T> {
    pattern: DraftField<T>,
    user_id: DraftField<T>,
    replacement: DraftField<T>,
}

fn render_nicknames_body<T: 'static>(
    rules: Vec<NicknameRule>,
    editor: EditorSnapshot<NicknameEditorState>,
    fields: NicknameTextFields<T>,
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
            "chat-nicknames-editor",
            "Nicknames",
            false,
            div()
                .flex()
                .flex_col()
                .gap_4()
                .child(
                    div()
                        .max_w(px(640.0))
                        .text_xs()
                        .text_color(colors::text_muted())
                        .child(
                            "Rename users locally. A Twitch user ID targets one account exactly; \
                             otherwise rules match the display name or login, exactly or as a \
                             regex. Regex replacements support capture groups like $1. Only \
                             affects how names render for you.",
                        ),
                )
                .child(render_editor(
                    editor,
                    editor_generation,
                    editor_needs_review,
                    fields,
                    cx,
                ))
                .child(render_rule_list(rules, cx)),
        ))
}

fn render_editor<T: 'static>(
    editor: NicknameEditorState,
    editor_generation: u64,
    editor_needs_review: bool,
    fields: NicknameTextFields<T>,
    cx: &mut Context<T>,
) -> impl IntoElement + use<T> {
    let editing = editor.editing_id.is_some();

    div()
        .flex()
        .flex_col()
        .gap_2()
        .child(
            div()
                .text_sm()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(colors::text_primary())
                .child(if editing {
                    "Edit Nickname"
                } else {
                    "Add Nickname"
                }),
        )
        .child(setting_target(NICKNAME_PATTERN, fields.pattern.render(cx)))
        .child(setting_target(NICKNAME_USER_ID, fields.user_id.render(cx)))
        .child(setting_target(
            NICKNAME_REPLACEMENT,
            fields.replacement.render(cx),
        ))
        .child(
            div()
                .flex()
                .gap_4()
                .items_center()
                .child(setting_target(
                    NICKNAME_REGEX,
                    render_flag_toggle("Regex", editor.regex, cx, |editor| {
                        editor.regex = !editor.regex;
                    }),
                ))
                .child(setting_target(
                    NICKNAME_CASE_SENSITIVE,
                    render_flag_toggle("Case sensitive", editor.case_sensitive, cx, |editor| {
                        editor.case_sensitive = !editor.case_sensitive;
                    }),
                )),
        )
        .child(render_editor_footer(
            div().flex().gap_2().items_center(),
            editing,
            editor_needs_review,
            editor.status,
            move |cx| {
                save_editor_rule(editor_generation);
                cx.notify();
            },
            reset_editor,
            cx,
        ))
}

fn render_rule_list<T: 'static>(
    rules: Vec<NicknameRule>,
    cx: &mut Context<T>,
) -> impl IntoElement + use<T> {
    div()
        .flex()
        .flex_col()
        .gap_2()
        .when(rules.is_empty(), |el| {
            el.child(empty_state("No nicknames configured."))
        })
        .children(rules.into_iter().map(|rule| render_rule_row(rule, cx)))
}

fn render_rule_row<T: 'static>(
    rule: NicknameRule,
    cx: &mut Context<T>,
) -> impl IntoElement + use<T> {
    let rule_for_toggle = rule.clone();
    let rule_for_edit = rule.clone();
    let rule_id_for_delete = rule.id.clone();
    let toggle_label = match rule.user_id.as_deref() {
        Some(user_id) if rule.pattern.is_empty() => {
            format!("Enable nickname rule for Twitch user {user_id}")
        }
        _ => format!("Enable nickname rule for {}", rule.pattern),
    };

    div()
        .w_full()
        .p_3()
        .bg(colors::bg_elevated())
        .rounded(Radii::md())
        .border_1()
        .border_color(colors::border())
        .flex()
        .flex_wrap()
        .items_start()
        .gap_3()
        .child(labelled_toggle(
            SharedString::from(format!("nickname-rule-toggle-{rule_id_for_delete}")),
            &toggle_label,
            rule.enabled,
            cx,
            move |_enabled, cx| {
                let mut updated = rule_for_toggle.clone();
                updated.enabled = !updated.enabled;
                settings::update_nickname_rule(&updated.id.clone(), updated);
                cx.notify();
            },
        ))
        .child(
            div().flex_1().min_w_0().flex().flex_col().gap_1().child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap_2()
                    .items_center()
                    .child(
                        div()
                            .min_w_0()
                            .text_sm()
                            .font_family("monospace")
                            .whitespace_normal()
                            .text_color(if rule.enabled {
                                colors::text_primary()
                            } else {
                                colors::text_muted()
                            })
                            .child(format!("{} → {}", rule.pattern, rule.replacement)),
                    )
                    .when_some(rule.user_id.as_deref(), |row, user_id| {
                        row.child(render_chip(format!("id:{user_id}")))
                    })
                    .when(rule.regex, |row| row.child(render_chip("regex")))
                    .when(rule.case_sensitive, |row| row.child(render_chip("Aa"))),
            ),
        )
        .child(render_button(
            "Edit",
            false,
            move |cx| {
                set_editor_for_rule(rule_for_edit.clone());
                cx.notify();
            },
            cx,
        ))
        .child(action_button_keyed(
            format!("delete-nickname-{rule_id_for_delete}"),
            "Delete",
            true,
            cx,
            move |cx| {
                settings::remove_nickname_rule(&rule_id_for_delete);
                if nickname_editor_snapshot().draft.editing_id.as_deref()
                    == Some(rule_id_for_delete.as_str())
                {
                    reset_editor();
                }
                cx.notify();
            },
        ))
}

fn render_chip(label: impl Into<SharedString>) -> impl IntoElement {
    div()
        .px_1p5()
        .py_0p5()
        .rounded(Radii::sm())
        .bg(colors::bg_surface())
        .border_1()
        .border_color(colors::border())
        .text_xs()
        .text_color(colors::text_secondary())
        .child(label.into())
}

fn render_flag_toggle<T: 'static, F: Fn(&mut NicknameEditorState) + 'static>(
    label: &'static str,
    is_on: bool,
    cx: &mut Context<T>,
    on_toggle: F,
) -> impl IntoElement + use<T, F> {
    let on_toggle = std::rc::Rc::new(on_toggle);
    let label_click = on_toggle.clone();
    div()
        .flex()
        .items_center()
        .gap_2()
        .child(labelled_toggle(
            SharedString::from(format!(
                "nickname-flag-{}",
                crate::components::settings::widgets::control_slug(label)
            )),
            label,
            is_on,
            cx,
            move |_enabled, cx| {
                update_editor(|editor| label_click(editor));
                cx.notify();
            },
        ))
        .child(
            div()
                .id(SharedString::from(format!(
                    "nickname-flag-label-{}",
                    crate::components::settings::widgets::control_slug(label)
                )))
                .cursor_pointer()
                .hover(|style| style.bg(colors::bg_hover()))
                .on_press_click(cx.listener(move |_this, _event, _window, cx| {
                    update_editor(|editor| on_toggle(editor));
                    cx.notify();
                }))
                .text_xs()
                .text_color(colors::text_secondary())
                .child(label),
        )
}

fn nickname_input<T: 'static>(
    key: &'static str,
    field: NicknameEditorField,
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
                    NicknameEditorField::Pattern => editor.pattern = text,
                    NicknameEditorField::UserId => editor.user_id = text,
                    NicknameEditorField::Replacement => editor.replacement = text,
                }
                editor.status = None;
            });
        })
        .on_enter(|_cx| save_editor_rule(nickname_editor_snapshot().generation))
        .on_escape(|_window, _cx| update_editor(|editor| editor.focused = None))
}

fn save_editor_rule(expected_generation: u64) {
    let Some(editor) = prepare_nickname_editor(expected_generation) else {
        return;
    };
    let pattern = editor.pattern.trim().to_string();
    let user_id = editor.user_id.trim().to_string();
    let replacement = editor.replacement.trim().to_string();

    if (pattern.is_empty() && user_id.is_empty()) || replacement.is_empty() {
        update_editor(|editor| {
            editor.status =
                Some("Username or Twitch user ID, and nickname are required".to_string())
        });
        return;
    }

    if user_id.is_empty()
        && editor.regex
        && let Err(error) = regex::Regex::new(&pattern)
    {
        let brief = error.to_string();
        let brief = brief.lines().last().unwrap_or("invalid regex").trim();
        update_editor(|editor| editor.status = Some(format!("Invalid regex: {brief}")));
        return;
    }

    let enabled = if let Some(editing_id) = editor.editing_id.as_deref() {
        let Some(rule) = settings::get_nickname_rules()
            .into_iter()
            .find(|rule| rule.id == editing_id)
        else {
            NICKNAME_EDITOR.replace_from_authority(NicknameEditorState {
                status: Some("Nickname no longer exists; the draft was reset".to_string()),
                ..NicknameEditorState::default()
            });
            return;
        };
        rule.enabled
    } else {
        true
    };

    let rule = NicknameRule {
        id: editor.editing_id.clone().unwrap_or_default(),
        pattern,
        replacement,
        user_id: (!user_id.is_empty()).then_some(user_id),
        regex: editor.regex,
        case_sensitive: editor.case_sensitive,
        enabled,
    };

    let status = if let Some(id) = editor.editing_id.as_deref() {
        settings::update_nickname_rule(id, rule);
        "Updated nickname"
    } else {
        settings::add_nickname_rule(rule);
        "Added nickname"
    };

    NICKNAME_EDITOR.replace_from_authority(NicknameEditorState {
        status: Some(status.to_string()),
        focused: editor.focused,
        ..NicknameEditorState::default()
    });
}

fn set_editor_for_rule(rule: NicknameRule) {
    NICKNAME_EDITOR.replace_from_authority(nickname_editor_from_rule(&rule));
}

fn reset_editor() {
    NICKNAME_EDITOR.replace_from_authority(NicknameEditorState::default());
}

pub(super) fn reset_nickname_editor_after_settings_replace() {
    NICKNAME_EDITOR.replace_from_authority(NicknameEditorState {
        status: Some("Nickname draft reset after settings were replaced".to_string()),
        ..NicknameEditorState::default()
    });
}

fn nickname_editor_from_rule(rule: &NicknameRule) -> NicknameEditorState {
    NicknameEditorState {
        pattern: rule.pattern.clone(),
        user_id: rule.user_id.clone().unwrap_or_default(),
        replacement: rule.replacement.clone(),
        regex: rule.regex,
        case_sensitive: rule.case_sensitive,
        editing_id: Some(rule.id.clone()),
        status: None,
        focused: None,
    }
}

fn recovered_nickname_editor_from(
    previous: &NicknameEditorState,
    rules: &[NicknameRule],
) -> NicknameEditorState {
    let mut editor = previous
        .editing_id
        .as_deref()
        .and_then(|editing_id| rules.iter().find(|rule| rule.id == editing_id))
        .map(nickname_editor_from_rule)
        .unwrap_or_default();
    editor.status = Some(
        "Nickname draft was reset after an internal editor error. Review or edit it before saving."
            .to_string(),
    );
    editor
}

fn recovered_nickname_editor(previous: &NicknameEditorState) -> NicknameEditorState {
    let rules = settings::get_nickname_rules();
    recovered_nickname_editor_from(previous, &rules)
}

fn nickname_editor_snapshot_from(rules: &[NicknameRule]) -> EditorSnapshot<NicknameEditorState> {
    NICKNAME_EDITOR.snapshot_or_reset(|previous| recovered_nickname_editor_from(previous, rules))
}

fn nickname_editor_snapshot() -> EditorSnapshot<NicknameEditorState> {
    NICKNAME_EDITOR.snapshot_or_reset(recovered_nickname_editor)
}

fn prepare_nickname_editor(expected_generation: u64) -> Option<NicknameEditorState> {
    NICKNAME_EDITOR.prepare_ready(expected_generation, recovered_nickname_editor)
}

fn update_editor(update: impl FnOnce(&mut NicknameEditorState)) {
    let _ = NICKNAME_EDITOR.apply_user_edit(recovered_nickname_editor, update);
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test;

    fn rule(id: &str, pattern: &str, user_id: Option<&str>, replacement: &str) -> NicknameRule {
        NicknameRule {
            id: id.to_string(),
            pattern: pattern.to_string(),
            replacement: replacement.to_string(),
            user_id: user_id.map(str::to_string),
            regex: true,
            case_sensitive: true,
            enabled: false,
        }
    }

    #[test]
    fn recovery_reloads_every_persisted_field_for_the_selected_id() {
        let previous = NicknameEditorState {
            pattern: "partial".to_string(),
            user_id: "partial".to_string(),
            replacement: "partial".to_string(),
            editing_id: Some("two".to_string()),
            focused: Some(NicknameEditorField::Replacement),
            ..NicknameEditorState::default()
        };
        let rules = [
            rule("one", "first", None, "First"),
            rule("two", "second.*", Some("123"), "Second"),
        ];

        let recovered = recovered_nickname_editor_from(&previous, &rules);
        assert_eq!(recovered.editing_id.as_deref(), Some("two"));
        assert_eq!(recovered.pattern, "second.*");
        assert_eq!(recovered.user_id, "123");
        assert_eq!(recovered.replacement, "Second");
        assert!(recovered.regex);
        assert!(recovered.case_sensitive);
        assert!(recovered.focused.is_none());
        assert!(recovered.status.is_some());
    }

    #[test]
    fn recovery_uses_a_marked_new_form_when_the_rule_was_removed() {
        let previous = NicknameEditorState {
            editing_id: Some("removed".to_string()),
            pattern: "partial".to_string(),
            ..NicknameEditorState::default()
        };

        let recovered = recovered_nickname_editor_from(&previous, &[]);
        assert_eq!(recovered.editing_id, None);
        assert!(recovered.pattern.is_empty());
        assert!(recovered.user_id.is_empty());
        assert!(recovered.replacement.is_empty());
        assert!(recovered.status.is_some());
    }
}
