//! Filters settings section.

use super::render_similarity_section;
use crate::backend::{self, FilterAction, FilterRule};
use crate::components::settings::draft_input::{self, DraftField, DraftInput};
use crate::components::settings::editor_state::{EditorSnapshot, EditorState};
use crate::components::settings::settings_navigation::{
    SettingDescriptor, SettingsSectionId, SettingsSubsectionId,
};
use crate::components::settings::settings_page::SettingsPage;
use crate::components::settings::widgets::{
    SelectOption, SettingsPageUi, SettingsSelectState, action_button_keyed,
    button as render_button, empty_state, labelled_toggle, quiet_action_button, setting_reset,
    setting_row, setting_row_with_control_width, setting_target, settings_defaults, settings_group,
    settings_group_contents, settings_select, settings_subheading, settings_subtopic,
    settings_summary_disclosure,
};
use crate::theme::colors;
use crate::theme::tokens::{Radii, RadiiPixels, Spacing};
use gpui::prelude::FluentBuilder;
use gpui::*;
use std::sync::LazyLock;
use std::sync::atomic::{AtomicUsize, Ordering};
use uuid::Uuid;

const FILTER_RULES_DISCLOSURE: &str = "filters-rules-editor";
const BLOCKED_USERS_DISCLOSURE: &str = "filters-blocked-users";

const EXCLUDE_SELF_FROM_FILTERS: SettingDescriptor = SettingDescriptor {
    id: "filters.exclude-self",
    label: "Exclude your messages from filters",
    aliases: &["active account filter exclusion"],
    control_id: "filters-exclude-self",
    disclosures: &[FILTER_RULES_DISCLOSURE],
};

const FILTER_EXPRESSION: SettingDescriptor = SettingDescriptor {
    id: "filters.expression",
    label: "Filter expression",
    aliases: &["filter rule match"],
    control_id: "filters-expression-field",
    disclosures: &[FILTER_RULES_DISCLOSURE],
};

const FILTER_REPLACEMENT: SettingDescriptor = SettingDescriptor {
    id: "filters.replacement",
    label: "Filter replacement",
    aliases: &["filter censor text", "filter mask"],
    control_id: "filters-replacement-field",
    disclosures: &[FILTER_RULES_DISCLOSURE],
};

const FILTER_IMPORT_EXPORT: SettingDescriptor = SettingDescriptor {
    id: "filters.import-export",
    label: "Import / export filters",
    aliases: &["filter JSON import", "filter backup export"],
    control_id: "filters-json-field",
    disclosures: &[FILTER_RULES_DISCLOSURE],
};

const FILTER_ACTION: SettingDescriptor = SettingDescriptor {
    id: "filters.action",
    label: "Filter action",
    aliases: &["highlight", "hide", "allow", "replace"],
    control_id: "filter-action-highlight",
    disclosures: &[FILTER_RULES_DISCLOSURE],
};

const FILTER_ENABLED: SettingDescriptor = SettingDescriptor {
    id: "filters.enabled",
    label: "Filter rule enabled",
    aliases: &["activate filter rule"],
    control_id: "filter-rule-enabled",
    disclosures: &[FILTER_RULES_DISCLOSURE],
};

const FILTER_REPLACEMENT_REGEX: SettingDescriptor = SettingDescriptor {
    id: "filters.replacement-regex",
    label: "Filter replacement regex",
    aliases: &["filter replacement regular expression"],
    control_id: "filters-replacement-regex",
    disclosures: &[FILTER_RULES_DISCLOSURE],
};

/// Filters › Filter rules.
pub const FILTERS_RULES_SETTINGS: &[SettingDescriptor] = &[
    EXCLUDE_SELF_FROM_FILTERS,
    FILTER_EXPRESSION,
    FILTER_REPLACEMENT,
    FILTER_IMPORT_EXPORT,
    FILTER_ACTION,
    FILTER_ENABLED,
    FILTER_REPLACEMENT_REGEX,
];

const BLOCKED_USERS: SettingDescriptor = SettingDescriptor {
    id: "filters.blocked-users",
    label: "Blocked users",
    aliases: &["twitch blocked-user list", "blocked-user management"],
    control_id: "blocked-users",
    disclosures: &[BLOCKED_USERS_DISCLOSURE],
};

const BLOCKED_USERS_ENABLED: SettingDescriptor = SettingDescriptor {
    id: "filters.blocked-users-enabled",
    label: "Use Twitch blocked users",
    aliases: &["twitch blocked-user filtering", "blocked whispers"],
    control_id: "filters-enable-twitch-blocked-users",
    disclosures: &[],
};

const BLOCKED_USERS_VISIBILITY: SettingDescriptor = SettingDescriptor {
    id: "filters.blocked-users-visibility",
    label: "Show blocked-user messages",
    aliases: &[
        "blocked-user message visibility",
        "blocked-user moderator visibility",
    ],
    control_id: "blocked-visibility-never",
    disclosures: &[],
};

/// Filters › Blocked users.
pub const FILTERS_BLOCKED_USERS_SETTINGS: &[SettingDescriptor] = &[
    BLOCKED_USERS,
    BLOCKED_USERS_ENABLED,
    BLOCKED_USERS_VISIBILITY,
];

#[derive(Clone, Copy, PartialEq, Eq)]
enum FilterEditorField {
    Expression,
    Replacement,
    ImportExportJson,
}

#[derive(Clone)]
struct FilterEditorState {
    expression: String,
    replacement: String,
    pattern_is_regex: bool,
    import_export_json: String,
    import_export_json_dirty: bool,
    editing_id: Option<String>,
    action: FilterAction,
    enabled: bool,
    status: Option<String>,
    status_is_error: bool,
    focused: Option<FilterEditorField>,
}

impl Default for FilterEditorState {
    fn default() -> Self {
        Self {
            expression: String::new(),
            replacement: String::new(),
            pattern_is_regex: false,
            import_export_json: String::new(),
            import_export_json_dirty: false,
            editing_id: None,
            action: FilterAction::Highlight,
            enabled: true,
            status: None,
            status_is_error: false,
            focused: None,
        }
    }
}

static FILTER_EDITOR: LazyLock<EditorState<FilterEditorState>> =
    LazyLock::new(|| EditorState::new("filters", FilterEditorState::default()));
const BLOCKED_USERS_PAGE_SIZE: usize = 50;
static BLOCKED_USERS_PAGE: AtomicUsize = AtomicUsize::new(0);

/// Render the Filters page as its three scroll anchors.
///
/// Similar messages is anchored here and nowhere else, so the feature has a
/// single place in the tree.
pub fn render_filters_section<T: 'static>(
    ui: &mut SettingsPageUi,
    window: &mut Window,
    cx: &mut Context<T>,
) -> SettingsPage {
    let filters = backend::get_filters();
    let editor = filter_editor_snapshot_from(&filters);
    let hide_blocked_user_details = backend::streamer_mode::should_hide_user_card_details();
    let blocked_status = backend::blocked_users::status();
    let rules_disclosure = ui.disclosure(FILTER_RULES_DISCLOSURE, cx);
    let blocked_disclosure = ui.disclosure(BLOCKED_USERS_DISCLOSURE, cx);
    let action_select = ui.select("filters-rule-action", cx);
    let blocked_visibility_select = ui.select("filters-blocked-visibility", cx);

    if !rules_disclosure.is_expanded() && editor.draft.focused.is_some() {
        update_editor(|editor| editor.focused = None);
    }
    let fields = FilterTextFields {
        expression: filter_input(
            FILTER_EXPRESSION.control_id,
            FilterEditorField::Expression,
            &editor.draft.expression,
            if editor.draft.action == FilterAction::Replace {
                "Pattern to match (literal text, or a regex when Regex is on)"
            } else {
                r#"Example: message.content contains "spoiler" && !author.subbed"#
            },
        )
        .label("Filter expression")
        .escape_focus(rules_disclosure.clone())
        .prepare(window, cx),
        replacement: filter_input(
            FILTER_REPLACEMENT.control_id,
            FilterEditorField::Replacement,
            &editor.draft.replacement,
            "Replacement text (e.g. ***)",
        )
        .escape_focus(rules_disclosure.clone())
        .prepare(window, cx),
        import_export: filter_input(
            FILTER_IMPORT_EXPORT.control_id,
            FilterEditorField::ImportExportJson,
            &editor.draft.import_export_json,
            "Paste or export filter JSON",
        )
        .escape_focus(rules_disclosure.clone())
        .prepare(window, cx),
    };

    SettingsPage::new(SettingsSectionId::Filters)
        .subsection(
            SettingsSubsectionId::FilterRules,
            render_filter_rules_settings(
                filters,
                editor,
                fields,
                &rules_disclosure,
                &action_select,
                ui.stacked,
                cx,
            ),
        )
        .subsection(
            SettingsSubsectionId::BlockedUsers,
            render_blocked_users_settings(
                blocked_status,
                hide_blocked_user_details,
                &blocked_disclosure,
                &blocked_visibility_select,
                ui.stacked,
                cx,
            ),
        )
        .subsection(
            SettingsSubsectionId::SimilarMessages,
            render_similarity_section(ui, window, cx),
        )
}

/// Text inputs for the rule editor, prepared while the window is available.
struct FilterTextFields<T> {
    expression: DraftField<T>,
    replacement: DraftField<T>,
    import_export: DraftField<T>,
}

fn render_filter_rules_settings<T: 'static>(
    filters: Vec<FilterRule>,
    editor: EditorSnapshot<FilterEditorState>,
    fields: FilterTextFields<T>,
    disclosure: &crate::components::settings::widgets::SettingsDisclosureState,
    action_select: &SettingsSelectState,
    stacked: bool,
    cx: &mut Context<T>,
) -> impl IntoElement + use<T> {
    let summary = filter_rules_summary(filters.len(), &editor);
    settings_group(
        "filter-rules",
        "Filters",
        false,
        settings_summary_disclosure(
            FILTER_RULES_DISCLOSURE,
            "Filter rules",
            &summary,
            disclosure,
            cx,
            move |cx| render_filter_rules_body(filters, editor, fields, action_select, stacked, cx),
        ),
    )
}

fn render_filter_rules_body<T: 'static>(
    filters: Vec<FilterRule>,
    editor: EditorSnapshot<FilterEditorState>,
    fields: FilterTextFields<T>,
    action_select: &SettingsSelectState,
    stacked: bool,
    cx: &mut Context<T>,
) -> impl IntoElement + use<T> {
    let editor_generation = editor.generation;
    let editor_needs_review = editor.needs_review;
    let editor_draft = editor.draft;
    let exclude_self = backend::settings::get_exclude_user_messages_from_filter();
    let default_exclude_self = settings_defaults().exclude_user_messages_from_filter;

    div()
        .w_full()
        .min_w_0()
        .flex()
        .flex_col()
        .gap_4()
        .child(settings_subtopic(
            "filters-rules-form",
            "Rules",
            div()
                .flex()
                .flex_col()
                .gap_4()
                .child(
                    div()
                        .max_w(px(640.0))
                        .text_sm()
                        .text_color(colors::text_secondary())
                        .child(
                            "Create rules to hide, allow, highlight, or replace matched chat messages.",
                        ),
                )
                .child(render_editor(
                    editor_draft,
                    editor_generation,
                    editor_needs_review,
                    fields.expression,
                    fields.replacement,
                    action_select,
                    cx,
                ))
                .child(render_filter_list(filters, stacked, cx)),
        ))
        .child(settings_subtopic(
            "filters-rule-options",
            "Options",
            setting_target(
                EXCLUDE_SELF_FROM_FILTERS,
                crate::components::settings::widgets::setting_compact_row(
                    EXCLUDE_SELF_FROM_FILTERS.id,
                    EXCLUDE_SELF_FROM_FILTERS.label,
                    Some("Always keep messages from the active Twitch account visible in filtered channel views."),
                    labelled_toggle(
                        EXCLUDE_SELF_FROM_FILTERS.control_id,
                        EXCLUDE_SELF_FROM_FILTERS.label,
                        exclude_self,
                        cx,
                        |excluded, cx| {
                            backend::settings::set_exclude_user_messages_from_filter(excluded);
                            cx.notify();
                        },
                    ),
                    exclude_self != default_exclude_self,
                    (exclude_self != default_exclude_self).then(|| {
                        setting_reset(
                            "reset-filters-exclude-self",
                            EXCLUDE_SELF_FROM_FILTERS.label,
                            cx,
                            move |cx| {
                                backend::settings::set_exclude_user_messages_from_filter(
                                    default_exclude_self,
                                );
                                cx.notify();
                            },
                        )
                    }),
                    stacked,
                ),
            ),
        ))
        .child(settings_subtopic(
            "filters-import-export",
            "Import & export",
            render_import_export(editor_generation, fields.import_export, cx),
        ))
}

fn render_blocked_users_settings<T: 'static>(
    status: backend::blocked_users::BlockedUsersStatus,
    hide_user_details: bool,
    disclosure: &crate::components::settings::widgets::SettingsDisclosureState,
    visibility_select: &SettingsSelectState,
    stacked: bool,
    cx: &mut Context<T>,
) -> impl IntoElement + use<T> {
    let enabled = backend::settings::get_enable_twitch_blocked_users();
    let visibility = backend::settings::get_show_blocked_users_messages();
    let defaults = settings_defaults();
    let visibility_value = blocked_visibility_value(visibility);
    let summary = blocked_users_summary(&status, hide_user_details);

    settings_group(
        "blocked-users",
        "Blocked users",
        false,
        settings_group_contents(
            "blocked-users-content",
            "Blocked users",
            true,
            div()
                .w_full()
                .min_w_0()
                .flex()
                .flex_col()
                .child(setting_target(
                    BLOCKED_USERS_ENABLED,
                    crate::components::settings::widgets::setting_compact_row(
                        BLOCKED_USERS_ENABLED.id,
                        BLOCKED_USERS_ENABLED.label,
                        Some("Hide messages and whispers from accounts blocked on Twitch."),
                        labelled_toggle(
                            BLOCKED_USERS_ENABLED.control_id,
                            BLOCKED_USERS_ENABLED.label,
                            enabled,
                            cx,
                            |enabled, cx| {
                                backend::settings::set_enable_twitch_blocked_users(enabled);
                                cx.notify();
                            },
                        ),
                        enabled != defaults.enable_twitch_blocked_users,
                        (enabled != defaults.enable_twitch_blocked_users).then(|| {
                            let value = defaults.enable_twitch_blocked_users;
                            setting_reset(
                                "reset-filters-blocked-users-enabled",
                                BLOCKED_USERS_ENABLED.label,
                                cx,
                                move |cx| {
                                    backend::settings::set_enable_twitch_blocked_users(value);
                                    cx.notify();
                                },
                            )
                        }),
                        stacked,
                    ),
                ))
                .child(setting_target(
                    BLOCKED_USERS_VISIBILITY,
                    setting_row_with_control_width(
                        BLOCKED_USERS_VISIBILITY.id,
                        BLOCKED_USERS_VISIBILITY.label,
                        Some("Choose when blocked messages stay visible. Moderator mode also includes channels you broadcast in. Whispers remain hidden because they have no channel role."),
                        settings_select(
                            BLOCKED_USERS_VISIBILITY.control_id,
                            BLOCKED_USERS_VISIBILITY.label,
                            &[
                                SelectOption::new("never", "Never"),
                                SelectOption::new("moderator", "While moderator"),
                                SelectOption::new("broadcaster", "While broadcaster"),
                            ],
                            visibility_value,
                            visibility_select,
                            cx,
                            |value, cx| {
                                backend::settings::set_show_blocked_users_messages(
                                    blocked_visibility_from_value(&value),
                                );
                                cx.notify();
                            },
                        ),
                        visibility != defaults.show_blocked_users_messages,
                        (visibility != defaults.show_blocked_users_messages).then(|| {
                            let value = defaults.show_blocked_users_messages;
                            setting_reset(
                                "reset-filters-blocked-users-visibility",
                                BLOCKED_USERS_VISIBILITY.label,
                                cx,
                                move |cx| {
                                    backend::settings::set_show_blocked_users_messages(value);
                                    cx.notify();
                                },
                            )
                        }),
                        stacked,
                        260.0,
                    ),
                ))
                .child(setting_target(
                    BLOCKED_USERS,
                    settings_summary_disclosure(
                        BLOCKED_USERS_DISCLOSURE,
                        "Manage blocked users",
                        &summary,
                        disclosure,
                        cx,
                        move |cx| {
                            render_blocked_users_body(status, hide_user_details, cx)
                        },
                    ),
                )),
        ),
    )
}

fn blocked_users_summary(
    status: &backend::blocked_users::BlockedUsersStatus,
    hide_user_details: bool,
) -> String {
    if hide_user_details {
        return "Details hidden while streamer mode is active".to_string();
    }
    match status {
        backend::blocked_users::BlockedUsersStatus::Loaded { count, .. } => match count {
            1 => "1 blocked".to_string(),
            count => format!("{count} blocked"),
        },
        backend::blocked_users::BlockedUsersStatus::Loading => {
            "Refreshing blocked users…".to_string()
        }
        backend::blocked_users::BlockedUsersStatus::Failed(_) => "Refresh failed".to_string(),
        backend::blocked_users::BlockedUsersStatus::NotLoaded => "Not loaded".to_string(),
    }
}

fn blocked_visibility_value(value: backend::settings::BlockedUserVisibility) -> &'static str {
    match value {
        backend::settings::BlockedUserVisibility::Never => "never",
        backend::settings::BlockedUserVisibility::IfModerator => "moderator",
        backend::settings::BlockedUserVisibility::IfBroadcaster => "broadcaster",
    }
}

fn blocked_visibility_from_value(value: &str) -> backend::settings::BlockedUserVisibility {
    match value {
        "moderator" => backend::settings::BlockedUserVisibility::IfModerator,
        "broadcaster" => backend::settings::BlockedUserVisibility::IfBroadcaster,
        _ => backend::settings::BlockedUserVisibility::Never,
    }
}

fn render_blocked_users_body<T: 'static>(
    status: backend::blocked_users::BlockedUsersStatus,
    hide_user_details: bool,
    cx: &mut Context<T>,
) -> impl IntoElement + use<T> {
    let requested_page = BLOCKED_USERS_PAGE.load(Ordering::Acquire);
    let users = if hide_user_details {
        backend::blocked_users::BlockedUserPage::default()
    } else {
        backend::blocked_users::snapshot_page(requested_page, BLOCKED_USERS_PAGE_SIZE)
    };
    BLOCKED_USERS_PAGE.store(users.page, Ordering::Release);
    let status_text = blocked_users_status_text(&status, &users, hide_user_details);
    let is_error = matches!(
        status,
        backend::blocked_users::BlockedUsersStatus::Failed(_)
    );

    div()
        .w_full()
        .min_w_0()
        .flex()
        .flex_col()
        .gap_3()
        .child(settings_subheading("Blocked-user list"))
        .child(
            div()
                .flex()
                .flex_wrap()
                .items_center()
                .justify_between()
                .gap_3()
                .child(
                    div()
                        .min_w_0()
                        .flex_1()
                        .text_xs()
                        .whitespace_normal()
                        .text_color(if is_error {
                            colors::signal_red()
                        } else {
                            colors::text_muted()
                        })
                        .child(status_text),
                )
                .child(render_button(
                    "Refresh",
                    false,
                    |cx| {
                        backend::blocked_users::refresh_current_account_async();
                        cx.notify();
                    },
                    cx,
                )),
        )
        .child(render_blocked_user_list(users, hide_user_details, cx))
        .child(
            div()
                .text_xs()
                .text_color(colors::text_muted())
                .child("Use /block <username> in chat to add a user; changes are applied to this list immediately."),
        )
}

fn blocked_users_status_text(
    status: &backend::blocked_users::BlockedUsersStatus,
    users: &backend::blocked_users::BlockedUserPage,
    hide_user_details: bool,
) -> String {
    if hide_user_details {
        return match status {
            backend::blocked_users::BlockedUsersStatus::NotLoaded => {
                "The Twitch block list has not loaded yet.".to_string()
            }
            backend::blocked_users::BlockedUsersStatus::Loading => {
                "Refreshing the Twitch block list; account details are hidden.".to_string()
            }
            backend::blocked_users::BlockedUsersStatus::Loaded { .. } => {
                "The Twitch block list is loaded; identities and counts are hidden.".to_string()
            }
            backend::blocked_users::BlockedUsersStatus::Failed(_) => {
                "The Twitch block-list refresh failed; details are hidden.".to_string()
            }
        };
    }
    match status {
        backend::blocked_users::BlockedUsersStatus::NotLoaded => {
            "The active account's Twitch block list has not loaded yet.".to_string()
        }
        backend::blocked_users::BlockedUsersStatus::Loading => {
            if users.total == 0 {
                "Refreshing the active account's Twitch block list…".to_string()
            } else {
                format!("Refreshing; showing {} cached user(s).", users.total)
            }
        }
        backend::blocked_users::BlockedUsersStatus::Loaded { count, truncated } => {
            if *truncated {
                format!(
                    "Showing the first {count} blocked users; the bounded in-memory list reached its limit."
                )
            } else {
                format!("Loaded {count} blocked user(s) from Twitch.")
            }
        }
        backend::blocked_users::BlockedUsersStatus::Failed(error) => {
            format!("Refresh failed: {error}")
        }
    }
}

fn render_blocked_user_list<T: 'static>(
    users: backend::blocked_users::BlockedUserPage,
    hide_user_details: bool,
    cx: &mut Context<T>,
) -> impl IntoElement + use<T> {
    let mut list = div().flex().flex_col().gap_2();
    if hide_user_details {
        return list.child(empty_state(
            "Blocked-user identities and per-user actions are hidden while streamer mode is active.",
        ));
    }
    if users.total == 0 {
        return list.child(empty_state("No blocked users are currently cached."));
    }

    let total = users.total;
    let page = users.page;
    let page_count = users.page_count;
    let start = page.saturating_mul(BLOCKED_USERS_PAGE_SIZE);
    let end = start.saturating_add(users.users.len());
    if users.page_count > 1 {
        list = list.child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .gap_3()
                .child(
                    div()
                        .text_xs()
                        .text_color(colors::text_muted())
                        .child(format!(
                            "Showing {}-{} of {} (page {} of {})",
                            start + 1,
                            end,
                            total,
                            page + 1,
                            page_count
                        )),
                )
                .child(
                    div()
                        .flex()
                        .gap_2()
                        .child(render_button(
                            "Previous",
                            false,
                            |cx| {
                                BLOCKED_USERS_PAGE
                                    .try_update(Ordering::AcqRel, Ordering::Acquire, |page| {
                                        Some(page.saturating_sub(1))
                                    })
                                    .ok();
                                cx.notify();
                            },
                            cx,
                        ))
                        .child(render_button(
                            "Next",
                            false,
                            move |cx| {
                                BLOCKED_USERS_PAGE
                                    .try_update(Ordering::AcqRel, Ordering::Acquire, |page| {
                                        Some((page + 1).min(page_count.saturating_sub(1)))
                                    })
                                    .ok();
                                cx.notify();
                            },
                            cx,
                        )),
                ),
        );
    }

    for user in users.users {
        let identity = if user.display_name.eq_ignore_ascii_case(&user.user_login) {
            format!("@{}", user.user_login)
        } else {
            format!("{} (@{})", user.display_name, user.user_login)
        };
        list = list.child(
            div()
                .w_full()
                .p_3()
                .bg(colors::bg_surface())
                .rounded(Radii::sm())
                .border_1()
                .border_color(colors::border())
                .flex()
                .items_center()
                .justify_between()
                .gap_3()
                .child(
                    div()
                        .min_w_0()
                        .text_sm()
                        .text_color(colors::text_primary())
                        .child(identity),
                )
                .child(action_button_keyed(
                    format!("unblock-twitch-user-{}", user.user_id),
                    "Unblock",
                    true,
                    cx,
                    move |cx| {
                        backend::blocked_users::unblock_user_async(user.clone());
                        cx.notify();
                    },
                )),
        );
    }
    list
}

fn filter_rules_summary(rule_count: usize, editor: &EditorSnapshot<FilterEditorState>) -> String {
    let rules = match rule_count {
        1 => "1 rule".to_string(),
        count => format!("{count} rules"),
    };
    let draft = &editor.draft;
    let has_draft = !draft.expression.is_empty()
        || !draft.replacement.is_empty()
        || draft.import_export_json_dirty
        || draft.pattern_is_regex
        || draft.action != FilterAction::Highlight
        || !draft.enabled;
    if editor.needs_review {
        format!("{rules} · Needs review")
    } else if draft.editing_id.is_some() {
        format!("{rules} · Editing…")
    } else if has_draft {
        format!("{rules} · Unsaved draft")
    } else {
        rules
    }
}

fn render_editor<T: 'static>(
    editor: FilterEditorState,
    editor_generation: u64,
    editor_needs_review: bool,
    expression_input: DraftField<T>,
    replacement_input: DraftField<T>,
    action_select: &SettingsSelectState,
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
                .child(if editing { "Edit rule" } else { "Add rule" }),
        )
        .child(setting_target(
            FILTER_EXPRESSION,
            expression_input.render(cx),
        ))
        .child(setting_target(
            FILTER_ACTION,
            setting_row(
                FILTER_ACTION.id,
                FILTER_ACTION.label,
                Some("Choose how messages matching this rule are handled."),
                settings_select(
                    FILTER_ACTION.control_id,
                    FILTER_ACTION.label,
                    &[
                        SelectOption::new("highlight", "Highlight"),
                        SelectOption::new("hide", "Hide"),
                        SelectOption::new("allow", "Allow"),
                        SelectOption::new("replace", "Replace"),
                    ],
                    filter_action_value(&editor.action),
                    action_select,
                    cx,
                    |value, cx| {
                        update_editor(|editor| {
                            editor.action = filter_action_from_value(&value);
                            editor.status = None;
                            editor.status_is_error = false;
                        });
                        cx.notify();
                    },
                ),
                false,
                None,
                true,
            ),
        ))
        .child(setting_target(
            FILTER_ENABLED,
            crate::components::settings::widgets::setting_compact_row(
                FILTER_ENABLED.id,
                FILTER_ENABLED.label,
                Some("Apply this rule while it remains saved."),
                labelled_toggle(
                    FILTER_ENABLED.control_id,
                    FILTER_ENABLED.label,
                    editor.enabled,
                    cx,
                    |enabled, cx| {
                        update_editor(|editor| {
                            editor.enabled = enabled;
                            editor.status = None;
                            editor.status_is_error = false;
                        });
                        cx.notify();
                    },
                ),
                false,
                None,
                true,
            ),
        ))
        .when(editor.action == FilterAction::Replace, |col| {
            col.child(setting_target(
                FILTER_REPLACEMENT,
                render_replace_controls(&editor, replacement_input, cx),
            ))
        })
        .child(
            div()
                .flex()
                .gap_2()
                .items_center()
                .child(render_button(
                    if editing { "Save" } else { "Add" },
                    true,
                    move |cx| {
                        save_editor_filter(editor_generation);
                        cx.notify();
                    },
                    cx,
                ))
                .when(editing, |row| {
                    row.child(render_button(
                        "Cancel",
                        false,
                        |cx| {
                            reset_editor();
                            cx.notify();
                        },
                        cx,
                    ))
                })
                .when_some(editor.status, |row, status| {
                    row.child(
                        div()
                            .text_xs()
                            .text_color(if editor.status_is_error || editor_needs_review {
                                colors::signal_red()
                            } else {
                                colors::text_muted()
                            })
                            .child(status),
                    )
                }),
        )
}

fn filter_action_value(action: &FilterAction) -> &'static str {
    match action {
        FilterAction::Highlight => "highlight",
        FilterAction::Hide => "hide",
        FilterAction::Allow => "allow",
        FilterAction::Replace => "replace",
    }
}

fn filter_action_from_value(value: &str) -> FilterAction {
    match value {
        "hide" => FilterAction::Hide,
        "allow" => FilterAction::Allow,
        "replace" => FilterAction::Replace,
        _ => FilterAction::Highlight,
    }
}

/// Replace-mode-only controls: the replacement text and the regex toggle.
fn render_replace_controls<T: 'static>(
    editor: &FilterEditorState,
    replacement_input: DraftField<T>,
    cx: &mut Context<T>,
) -> impl IntoElement + use<T> {
    div()
        .flex()
        .flex_col()
        .gap_2()
        .child(
            div()
                .max_w(px(640.0))
                .text_xs()
                .text_color(colors::text_muted())
                .child(
                    "Replace mode rewrites matched text in the message. The pattern is matched \
                     case-insensitively and every occurrence is replaced. Leave the replacement \
                     empty to remove the matched text.",
                ),
        )
        .child(replacement_input.render(cx))
        .child(setting_target(
            FILTER_REPLACEMENT_REGEX,
            crate::components::settings::widgets::setting_compact_row(
                FILTER_REPLACEMENT_REGEX.id,
                "Regex",
                None,
                labelled_toggle(
                    FILTER_REPLACEMENT_REGEX.control_id,
                    FILTER_REPLACEMENT_REGEX.label,
                    editor.pattern_is_regex,
                    cx,
                    |is_regex, cx| {
                        update_editor(|editor| {
                            editor.pattern_is_regex = is_regex;
                            editor.status = None;
                            editor.status_is_error = false;
                        });
                        cx.notify();
                    },
                ),
                false,
                None,
                false,
            ),
        ))
}

fn render_filter_list<T: 'static>(
    filters: Vec<FilterRule>,
    stacked: bool,
    cx: &mut Context<T>,
) -> impl IntoElement + use<T> {
    div()
        .flex()
        .flex_col()
        .gap_2()
        .when(filters.is_empty(), |el| {
            el.child(empty_state("No filter rules configured"))
        })
        .children(
            filters
                .into_iter()
                .map(|rule| render_filter_rule(rule, stacked, cx)),
        )
}

/// Render a single filter rule.
fn render_filter_rule<T: 'static>(
    rule: FilterRule,
    stacked: bool,
    cx: &mut Context<T>,
) -> impl IntoElement + use<T> {
    let action_color = action_color(&rule.action);
    let action_label = action_label(&rule.action);
    let is_enabled = rule.enabled;
    let is_replace = rule.action == FilterAction::Replace;
    let replacement_preview = rule.replacement.clone().unwrap_or_default();
    let rule_for_toggle = rule.clone();
    let toggle_label = format!("Enable filter {}", rule.expression);
    let rule_for_edit = rule.clone();
    let rule_id_for_delete = rule.id.clone();

    div()
        .w_full()
        .p_2()
        .rounded(Radii::sm())
        .border_1()
        .border_color(colors::border())
        .flex()
        .flex_wrap()
        .items_center()
        .gap_3()
        .child(labelled_toggle(
            format!("filter-rule-toggle-{rule_id_for_delete}"),
            &toggle_label,
            is_enabled,
            cx,
            move |enabled, cx| {
                let mut updated_rule = rule_for_toggle.clone();
                updated_rule.enabled = enabled;
                backend::update_filter(&updated_rule.id.clone(), updated_rule);
                cx.notify();
            },
        ))
        .child(
            div()
                .min_w_0()
                .flex_basis(px(260.0))
                .flex_1()
                .flex()
                .flex_col()
                .gap_1()
                .child(
                    div()
                        .text_sm()
                        .font_family("monospace")
                        .text_color(if is_enabled {
                            colors::text_primary()
                        } else {
                            colors::text_muted()
                        })
                        .child(rule.expression.clone()),
                )
                .child(
                    div()
                        .flex()
                        .gap_2()
                        .items_center()
                        .child(
                            div()
                                .px_1p5()
                                .py(px(Spacing::XXS))
                                .rounded(Radii::sm())
                                .bg(action_color.opacity(0.2))
                                .text_xs()
                                .text_color(action_color)
                                .child(action_label),
                        )
                        .when(is_replace, |row| {
                            row.child(
                                div()
                                    .min_w_0()
                                    .text_xs()
                                    .text_color(colors::text_muted())
                                    .child(if replacement_preview.is_empty() {
                                        "\u{2192} (removed)".to_string()
                                    } else {
                                        format!("\u{2192} {replacement_preview}")
                                    }),
                            )
                        }),
                ),
        )
        .child(quiet_action_button(
            format!("edit-filter-{rule_id_for_delete}"),
            "Edit",
            false,
            cx,
            move |cx| {
                set_editor_for_filter(rule_for_edit.clone());
                cx.notify();
            },
        ))
        .child(
            div()
                .when(stacked, |actions| actions.w_full().flex().justify_end())
                .child(quiet_action_button(
                    format!("delete-filter-{rule_id_for_delete}"),
                    "Delete",
                    true,
                    cx,
                    move |cx| {
                        backend::remove_filter(&rule_id_for_delete);
                        if filter_editor_snapshot().draft.editing_id.as_deref()
                            == Some(rule_id_for_delete.as_str())
                        {
                            reset_editor();
                        }
                        cx.notify();
                    },
                )),
        )
}

fn render_import_export<T: 'static>(
    editor_generation: u64,
    json_input: DraftField<T>,
    cx: &mut Context<T>,
) -> impl IntoElement + use<T> {
    div()
        .flex()
        .flex_col()
        .gap_2()
        .child(setting_target(FILTER_IMPORT_EXPORT, json_input.render(cx)))
        .child(
            div()
                .flex()
                .gap_2()
                .child(render_button(
                    "Export JSON",
                    false,
                    |cx| {
                        let status = match backend::settings::export_filters_to_json() {
                            Ok(json) => {
                                crate::utils::copy_text(&json);
                                update_editor(|editor| {
                                    editor.import_export_json = json;
                                    editor.import_export_json_dirty = false;
                                    editor.status = Some("Exported filters".to_string());
                                    editor.status_is_error = false;
                                });
                                None
                            }
                            Err(error) => Some(error),
                        };
                        if let Some(error) = status {
                            update_editor(|editor| {
                                editor.status = Some(error);
                                editor.status_is_error = true;
                            });
                        }
                        cx.notify();
                    },
                    cx,
                ))
                .child(render_button(
                    "Import JSON",
                    false,
                    move |cx| {
                        if let Some(editor) = prepare_filter_editor(editor_generation) {
                            let status = match backend::settings::import_filters_from_json(
                                &editor.import_export_json,
                            ) {
                                Ok(count) => (format!("Imported {count} filters"), false),
                                Err(error) => (error, true),
                            };
                            update_editor(|editor| {
                                editor.status = Some(status.0);
                                editor.status_is_error = status.1;
                                if !status.1 {
                                    editor.import_export_json.clear();
                                    editor.import_export_json_dirty = false;
                                }
                            });
                        }
                        cx.notify();
                    },
                    cx,
                ))
                .child(render_button(
                    "Clear",
                    false,
                    |cx| {
                        update_editor(|editor| {
                            editor.import_export_json.clear();
                            editor.import_export_json_dirty = false;
                            editor.status = None;
                            editor.status_is_error = false;
                        });
                        cx.notify();
                    },
                    cx,
                )),
        )
}

fn filter_input<T: 'static>(
    key: &'static str,
    field: FilterEditorField,
    value: &str,
    placeholder: &'static str,
) -> DraftInput<T> {
    DraftInput::new(key, value, placeholder)
        .monospace(true)
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
                    FilterEditorField::Expression => editor.expression = text,
                    FilterEditorField::Replacement => editor.replacement = text,
                    FilterEditorField::ImportExportJson => {
                        editor.import_export_json = text;
                        editor.import_export_json_dirty = true;
                    }
                }
                editor.status = None;
                editor.status_is_error = false;
            });
        })
        .on_enter(move |_cx| {
            if field != FilterEditorField::ImportExportJson {
                save_editor_filter(filter_editor_snapshot().generation);
            }
        })
        .on_escape(|_window, _cx| update_editor(|editor| editor.focused = None))
}

fn save_editor_filter(expected_generation: u64) {
    let Some(editor) = prepare_filter_editor(expected_generation) else {
        return;
    };
    let expression = editor.expression.trim().to_string();
    let is_replace = editor.action == FilterAction::Replace;

    if expression.is_empty() {
        update_editor(|editor| {
            editor.status = Some(if is_replace {
                "Pattern is required".to_string()
            } else {
                "Expression is required".to_string()
            });
            editor.status_is_error = true;
        });
        return;
    }

    if is_replace {
        if editor.pattern_is_regex
            && let Err(error) = regex::Regex::new(&expression)
        {
            update_editor(|editor| {
                editor.status = Some(format!(
                    "Invalid regex: {}",
                    super::brief_regex_error(&error)
                ));
                editor.status_is_error = true;
            });
            return;
        }
    } else if let Err(error) = backend::filters::validate_filter_expression(&expression) {
        update_editor(|editor| {
            editor.status = Some(format!("Invalid expression: {error}"));
            editor.status_is_error = true;
        });
        return;
    }

    let rule = FilterRule {
        id: editor
            .editing_id
            .clone()
            .unwrap_or_else(|| Uuid::new_v4().to_string()),
        expression,
        action: editor.action.clone(),
        enabled: editor.enabled,
        replacement: if is_replace {
            Some(editor.replacement.clone())
        } else {
            None
        },
        pattern_is_regex: is_replace && editor.pattern_is_regex,
    };

    let status = if let Some(editing_id) = editor.editing_id.as_deref() {
        if !backend::get_filters()
            .iter()
            .any(|existing| existing.id == editing_id)
        {
            FILTER_EDITOR.replace_from_authority(FilterEditorState {
                status: Some("Filter no longer exists; the draft was reset".to_string()),
                status_is_error: true,
                ..FilterEditorState::default()
            });
            return;
        }
        backend::update_filter(&rule.id.clone(), rule);
        "Updated filter"
    } else {
        backend::add_filter(rule);
        "Added filter"
    };

    set_filter_editor_state(FilterEditorState {
        import_export_json: editor.import_export_json,
        import_export_json_dirty: editor.import_export_json_dirty,
        status: Some(status.to_string()),
        focused: editor.focused,
        ..FilterEditorState::default()
    });
}

fn set_editor_for_filter(rule: FilterRule) {
    let current = filter_editor_snapshot().draft;
    let mut editor = filter_editor_from_rule(&rule);
    editor.import_export_json = current.import_export_json;
    editor.import_export_json_dirty = current.import_export_json_dirty;
    editor.focused = Some(FilterEditorField::Expression);
    FILTER_EDITOR.replace_from_authority(editor);
    draft_input::request_focus(FILTER_EXPRESSION.control_id);
}

fn reset_editor() {
    set_filter_editor_state(FilterEditorState::default());
}

pub(super) fn reset_filter_editor_after_settings_replace() {
    set_filter_editor_state(FilterEditorState {
        status: Some("Filter draft reset after settings were replaced".to_string()),
        ..FilterEditorState::default()
    });
}

fn filter_editor_from_rule(rule: &FilterRule) -> FilterEditorState {
    let is_replace = rule.action == FilterAction::Replace;
    FilterEditorState {
        expression: rule.expression.clone(),
        replacement: if is_replace {
            rule.replacement.clone().unwrap_or_default()
        } else {
            String::new()
        },
        pattern_is_regex: is_replace && rule.pattern_is_regex,
        import_export_json: String::new(),
        import_export_json_dirty: false,
        editing_id: Some(rule.id.clone()),
        action: rule.action.clone(),
        enabled: rule.enabled,
        status: None,
        status_is_error: false,
        focused: None,
    }
}

fn recovered_filter_editor_from(
    previous: &FilterEditorState,
    filters: &[FilterRule],
) -> FilterEditorState {
    let mut editor = previous
        .editing_id
        .as_deref()
        .and_then(|editing_id| filters.iter().find(|rule| rule.id == editing_id))
        .map(filter_editor_from_rule)
        .unwrap_or_default();
    editor.status = Some(
        "Filter draft was reset after an internal editor error. Review or edit it before saving."
            .to_string(),
    );
    editor.status_is_error = true;
    editor
}

fn recovered_filter_editor(previous: &FilterEditorState) -> FilterEditorState {
    let filters = backend::get_filters();
    recovered_filter_editor_from(previous, &filters)
}

fn filter_editor_snapshot_from(filters: &[FilterRule]) -> EditorSnapshot<FilterEditorState> {
    FILTER_EDITOR.snapshot_or_reset(|previous| recovered_filter_editor_from(previous, filters))
}

fn filter_editor_snapshot() -> EditorSnapshot<FilterEditorState> {
    FILTER_EDITOR.snapshot_or_reset(recovered_filter_editor)
}

fn prepare_filter_editor(expected_generation: u64) -> Option<FilterEditorState> {
    FILTER_EDITOR.prepare_ready(expected_generation, recovered_filter_editor)
}

fn set_filter_editor_state(state: FilterEditorState) {
    FILTER_EDITOR.replace_from_authority(state);
}

fn update_editor(update: impl FnOnce(&mut FilterEditorState)) {
    let _ = FILTER_EDITOR.apply_user_edit(recovered_filter_editor, update);
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test;

    fn rule(id: &str, action: FilterAction) -> FilterRule {
        let is_replace = action == FilterAction::Replace;
        FilterRule {
            id: id.to_string(),
            expression: format!("expression-{id}"),
            action,
            enabled: false,
            replacement: is_replace.then(|| "replacement".to_string()),
            pattern_is_regex: is_replace,
        }
    }

    #[test]
    fn recovery_reloads_every_replace_field_for_the_selected_id() {
        let previous = FilterEditorState {
            expression: "partial".to_string(),
            replacement: "partial".to_string(),
            import_export_json: "unsaved".to_string(),
            editing_id: Some("two".to_string()),
            focused: Some(FilterEditorField::Replacement),
            ..FilterEditorState::default()
        };
        let filters = [
            rule("one", FilterAction::Hide),
            rule("two", FilterAction::Replace),
        ];

        let recovered = recovered_filter_editor_from(&previous, &filters);
        assert_eq!(recovered.editing_id.as_deref(), Some("two"));
        assert_eq!(recovered.expression, "expression-two");
        assert_eq!(recovered.replacement, "replacement");
        assert!(recovered.pattern_is_regex);
        assert_eq!(recovered.action, FilterAction::Replace);
        assert!(!recovered.enabled);
        assert!(recovered.import_export_json.is_empty());
        assert!(recovered.focused.is_none());
        assert!(recovered.status_is_error);
    }

    #[test]
    fn non_replace_recovery_clears_irrelevant_replace_state() {
        let mut persisted = rule("one", FilterAction::Hide);
        persisted.replacement = Some("stale".to_string());
        persisted.pattern_is_regex = true;
        let previous = FilterEditorState {
            editing_id: Some("one".to_string()),
            ..FilterEditorState::default()
        };

        let recovered = recovered_filter_editor_from(&previous, &[persisted]);
        assert!(recovered.replacement.is_empty());
        assert!(!recovered.pattern_is_regex);
        assert_eq!(recovered.action, FilterAction::Hide);
    }

    #[test]
    fn recovery_uses_a_marked_new_form_when_the_rule_was_removed() {
        let previous = FilterEditorState {
            editing_id: Some("removed".to_string()),
            expression: "partial".to_string(),
            ..FilterEditorState::default()
        };

        let recovered = recovered_filter_editor_from(&previous, &[]);
        assert_eq!(recovered.editing_id, None);
        assert!(recovered.expression.is_empty());
        assert_eq!(recovered.action, FilterAction::Highlight);
        assert!(recovered.status_is_error);
    }

    #[test]
    fn rules_summary_prioritizes_needs_review_over_editing() {
        let editor = EditorSnapshot {
            draft: FilterEditorState {
                editing_id: Some("editing".to_string()),
                ..FilterEditorState::default()
            },
            generation: 0,
            needs_review: true,
        };

        assert_eq!(filter_rules_summary(1, &editor), "1 rule · Needs review");
    }

    #[test]
    fn exported_json_does_not_mark_the_rule_editor_as_a_draft() {
        let editor = EditorSnapshot {
            draft: FilterEditorState {
                import_export_json: r#"[{"expression":"hello"}]"#.to_string(),
                import_export_json_dirty: false,
                ..FilterEditorState::default()
            },
            generation: 0,
            needs_review: false,
        };

        assert_eq!(filter_rules_summary(2, &editor), "2 rules");
    }

    #[test]
    fn typed_json_marks_the_rule_editor_as_a_draft_until_cleared() {
        let mut editor = EditorSnapshot {
            draft: FilterEditorState {
                import_export_json: "{\"filters\":[]}".to_string(),
                import_export_json_dirty: true,
                ..FilterEditorState::default()
            },
            generation: 0,
            needs_review: false,
        };

        assert_eq!(filter_rules_summary(0, &editor), "0 rules · Unsaved draft");
        editor.draft.import_export_json.clear();
        editor.draft.import_export_json_dirty = false;
        assert_eq!(filter_rules_summary(0, &editor), "0 rules");
    }

    #[test]
    fn streamer_mode_blocked_summary_never_includes_the_count() {
        let status = backend::blocked_users::BlockedUsersStatus::Loaded {
            count: 37,
            truncated: false,
        };

        assert_eq!(
            blocked_users_summary(&status, true),
            "Details hidden while streamer mode is active"
        );
    }
}

fn action_label(action: &FilterAction) -> &'static str {
    match action {
        FilterAction::Allow => "Allow",
        FilterAction::Hide => "Hide",
        FilterAction::Highlight => "Highlight",
        FilterAction::Replace => "Replace",
    }
}

fn action_color(action: &FilterAction) -> Hsla {
    match action {
        FilterAction::Allow => colors::signal_green(),
        FilterAction::Hide => colors::signal_red(),
        FilterAction::Highlight => colors::accent(),
        FilterAction::Replace => colors::signal_yellow(),
    }
}
