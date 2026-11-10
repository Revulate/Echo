//! Similar-message detection settings.

use crate::backend::similarity::{
    self, get_similarity_settings, set_similarity_color_similar, set_similarity_enabled,
    set_similarity_hide_similar, set_similarity_max_delay_secs,
    set_similarity_max_messages_to_check, set_similarity_same_user_only, set_similarity_threshold,
};
use crate::components::settings::editor_state::{EditorSnapshot, EditorState};
use crate::components::settings::settings_navigation::SettingDescriptor;
use crate::components::settings::widgets::{
    SettingsPageUi, labelled_toggle, setting_float_modified, setting_reset,
    setting_row_with_enabled, setting_target, settings_disclosure_with_layout, settings_group,
    settings_group_contents, settings_subheading, toggle_with_enabled,
};
use crate::components::ui::ButtonExt;
use crate::components::ui::input::{Escape, Input, InputEvent, InputState};
use crate::theme::tokens::RadiiPixels;
use gpui::prelude::FluentBuilder;
use gpui::*;
use std::{cell::RefCell, sync::LazyLock};

const OPTIONS_DISCLOSURE: &str = "filters-similarity-options";

const SIMILARITY_ENABLED: SettingDescriptor = SettingDescriptor {
    id: "filters.similarity-enabled",
    label: "Enable detection",
    aliases: &["similar", "duplicate", "spam", "repeated"],
    control_id: "similarity-enabled",
    disclosures: &[],
};

const SIMILARITY_THRESHOLD: SettingDescriptor = SettingDescriptor {
    id: "filters.similarity-threshold",
    label: "Similarity threshold",
    aliases: &["percent", "strict", "loose", "exact"],
    control_id: "similarity-threshold",
    disclosures: &[OPTIONS_DISCLOSURE],
};

const SIMILARITY_HIDE: SettingDescriptor = SettingDescriptor {
    id: "filters.similarity-hide",
    label: "Hide similar messages",
    aliases: &["hide", "remove", "duplicate"],
    control_id: "hide-similar",
    disclosures: &[OPTIONS_DISCLOSURE],
};

const SIMILARITY_SHADE: SettingDescriptor = SettingDescriptor {
    id: "filters.similarity-shade",
    label: "Shade similar messages",
    aliases: &["dim", "color", "gray"],
    control_id: "color-similar",
    disclosures: &[OPTIONS_DISCLOSURE],
};

const SIMILARITY_SAME_USER: SettingDescriptor = SettingDescriptor {
    id: "filters.similarity-same-user",
    label: "Same user only",
    aliases: &["per user", "sender"],
    control_id: "same-user",
    disclosures: &[OPTIONS_DISCLOSURE],
};

const SIMILARITY_MAX_DELAY: SettingDescriptor = SettingDescriptor {
    id: "filters.similarity-max-delay",
    label: "Max delay",
    aliases: &["Max delay (seconds)", "window", "time", "seconds"],
    control_id: "similarity-max-delay",
    disclosures: &[OPTIONS_DISCLOSURE],
};

const SIMILARITY_MAX_MESSAGES: SettingDescriptor = SettingDescriptor {
    id: "filters.similarity-max-messages",
    label: "Messages to check",
    aliases: &["history", "lookback", "compare"],
    control_id: "similarity-max-messages",
    disclosures: &[OPTIONS_DISCLOSURE],
};

/// Filters › Similar messages.
pub const FILTERS_SIMILAR_MESSAGES_SETTINGS: &[SettingDescriptor] = &[
    SIMILARITY_ENABLED,
    SIMILARITY_THRESHOLD,
    SIMILARITY_HIDE,
    SIMILARITY_SHADE,
    SIMILARITY_SAME_USER,
    SIMILARITY_MAX_DELAY,
    SIMILARITY_MAX_MESSAGES,
];

#[derive(Clone, Default)]
struct SimilarityThresholdEditor {
    text: String,
    focused: bool,
    dirty: bool,
    invalid: bool,
}

static THRESHOLD_EDITOR: LazyLock<EditorState<SimilarityThresholdEditor>> = LazyLock::new(|| {
    EditorState::new("similarity threshold", SimilarityThresholdEditor::default())
});

thread_local! {
    static THRESHOLD_INPUT: RefCell<Option<SimilarityThresholdInput>> = const { RefCell::new(None) };
}

struct SimilarityThresholdInput {
    owner: EntityId,
    input: Entity<InputState>,
    _subscription: Subscription,
}

/// Render the Similar messages anchor body (Filters › Similar messages).
pub fn render_similarity_section<T: 'static>(
    ui: &mut SettingsPageUi,
    window: &mut Window,
    cx: &mut Context<T>,
) -> impl IntoElement + use<T> {
    let settings = get_similarity_settings();
    let defaults = similarity::SimilaritySettings::default();
    let disclosure = ui.disclosure(OPTIONS_DISCLOSURE, cx);
    let snapshot = threshold_editor_snapshot(settings.threshold);
    let threshold_needs_review = snapshot.needs_review;
    let threshold_editor = snapshot.draft;

    if !disclosure.is_expanded() && threshold_editor.focused {
        update_threshold_editor(settings.threshold, |editor| editor.focused = false);
    }
    if !disclosure.is_expanded() {
        release_threshold_input();
    }

    let options_modified = similarity_options_modified(&settings, &defaults);
    let summary = threshold_summary(&threshold_editor, threshold_needs_review);
    let stacked = ui.stacked;

    settings_group(
        "similar-messages",
        "Similar messages",
        false,
        settings_group_contents(
            "similar-messages-content",
            "Similar messages",
            true,
            div()
                .w_full()
                .min_w_0()
                .flex()
                .flex_col()
                .child(setting_target(
                    SIMILARITY_ENABLED,
                    crate::components::settings::widgets::setting_compact_row(
                        SIMILARITY_ENABLED.id,
                        SIMILARITY_ENABLED.label,
                        Some("Detect and mark similar or duplicate chat messages."),
                        labelled_toggle(
                            SIMILARITY_ENABLED.control_id,
                            SIMILARITY_ENABLED.label,
                            settings.enabled,
                            cx,
                            |enabled, cx| {
                                set_similarity_enabled(enabled);
                                cx.notify();
                            },
                        ),
                        settings.enabled != defaults.enabled,
                        (settings.enabled != defaults.enabled).then(|| {
                            setting_reset(
                                "reset-similarity-enabled",
                                SIMILARITY_ENABLED.label,
                                cx,
                                move |cx| {
                                    set_similarity_enabled(defaults.enabled);
                                    cx.notify();
                                },
                            )
                        }),
                        stacked,
                    ),
                ))
                .child(settings_disclosure_with_layout(
                    OPTIONS_DISCLOSURE,
                    "Detection options",
                    Some(&summary),
                    options_modified,
                    &disclosure,
                    stacked,
                    cx,
                    move |cx| {
                        render_options(
                            &settings,
                            &defaults,
                            threshold_editor,
                            threshold_needs_review,
                            stacked,
                            window,
                            cx,
                        )
                    },
                )),
        ),
    )
}

fn render_options<T: 'static>(
    settings: &similarity::SimilaritySettings,
    defaults: &similarity::SimilaritySettings,
    threshold_editor: SimilarityThresholdEditor,
    threshold_needs_review: bool,
    stacked: bool,
    window: &mut Window,
    cx: &mut Context<T>,
) -> impl IntoElement + use<T> {
    div()
        .w_full()
        .min_w_0()
        .flex()
        .flex_col()
        .child(settings_subheading("Detection"))
        .child(setting_target(
            SIMILARITY_THRESHOLD,
            render_threshold_row(
                (settings.threshold, defaults.threshold),
                settings.enabled,
                threshold_editor,
                threshold_needs_review,
                stacked,
                window,
                cx,
            ),
        ))
        .child(setting_target(
            SIMILARITY_SAME_USER,
            render_toggle_row(
                SIMILARITY_SAME_USER,
                "Only compare messages sent by the same user.",
                (settings.same_user_only, defaults.same_user_only),
                settings.enabled,
                stacked,
                cx,
                set_similarity_same_user_only,
            ),
        ))
        .child(setting_target(
            SIMILARITY_MAX_DELAY,
            render_number_row(
                SIMILARITY_MAX_DELAY,
                "Maximum time between messages to compare.",
                settings.max_delay_secs,
                defaults.max_delay_secs,
                1,
                60,
                "seconds",
                settings.enabled,
                stacked,
                cx,
                set_similarity_max_delay_secs,
            ),
        ))
        .child(setting_target(
            SIMILARITY_MAX_MESSAGES,
            render_number_row(
                SIMILARITY_MAX_MESSAGES,
                "How many recent messages to compare against.",
                settings.max_messages_to_check,
                defaults.max_messages_to_check,
                1,
                20,
                "messages",
                settings.enabled,
                stacked,
                cx,
                set_similarity_max_messages_to_check,
            ),
        ))
        .child(settings_subheading("Display"))
        .child(setting_target(
            SIMILARITY_HIDE,
            render_toggle_row(
                SIMILARITY_HIDE,
                "Remove messages detected as similar from the chat view.",
                (settings.hide_similar, defaults.hide_similar),
                settings.enabled,
                stacked,
                cx,
                set_similarity_hide_similar,
            ),
        ))
        .child(setting_target(
            SIMILARITY_SHADE,
            render_toggle_row(
                SIMILARITY_SHADE,
                "Dim repeated messages when they remain visible.",
                (settings.color_similar, defaults.color_similar),
                settings.enabled && !settings.hide_similar,
                stacked,
                cx,
                set_similarity_color_similar,
            ),
        ))
}

fn render_threshold_row<T: 'static>(
    (threshold, default): (f32, f32),
    enabled: bool,
    editor: SimilarityThresholdEditor,
    needs_review: bool,
    stacked: bool,
    window: &mut Window,
    cx: &mut Context<T>,
) -> impl IntoElement + use<T> {
    let input = ensure_threshold_input(threshold, window, cx);
    sync_threshold_input_text(threshold, false, window, cx);
    let modified = setting_float_modified(threshold, default);
    let help = if editor.invalid {
        "Invalid threshold. Enter a finite value from 0 to 1; use a period for decimals."
    } else if needs_review {
        "Needs review. Enter an exact value from 0 to 1."
    } else if editor.dirty {
        "Unsaved draft. Press Enter to apply an exact value from 0 to 1."
    } else {
        "Enter an exact value from 0 to 1; higher values are stricter."
    };

    setting_row_with_enabled(
        SIMILARITY_THRESHOLD.id,
        SIMILARITY_THRESHOLD.label,
        Some(help),
        div()
            .w_full()
            .min_w_0()
            .flex()
            .items_center()
            .gap_2()
            .child(
                div()
                    .min_w(px(104.0))
                    .flex_1()
                    .capture_action(cx.listener(move |_this, _: &Escape, window, cx| {
                        let authority = get_similarity_settings().threshold;
                        reset_threshold_editor(authority);
                        sync_threshold_input_text(authority, true, window, cx);
                        cx.stop_propagation();
                        cx.notify();
                    }))
                    .child(if enabled {
                        div()
                            .id("similarity-threshold-input")
                            .w_full()
                            .h(px(32.0))
                            .child(
                                Input::new(&input)
                                    .accessibility_label("Similarity threshold from 0 to 1"),
                            )
                            .into_any_element()
                    } else {
                        div()
                            .id("similarity-threshold-input-disabled")
                            .w_full()
                            .h(px(32.0))
                            .px_2()
                            .flex()
                            .items_center()
                            .rounded(crate::theme::tokens::Radii::sm())
                            .bg(crate::theme::colors::bg_surface())
                            .border_1()
                            .border_color(crate::theme::colors::border())
                            .text_sm()
                            .text_color(crate::theme::colors::text_muted())
                            .child(editor.text)
                            .into_any_element()
                    }),
            )
            .child(
                div()
                    .flex_shrink_0()
                    .text_xs()
                    .text_color(crate::theme::colors::text_secondary())
                    .child(percent_text(threshold)),
            )
            .child(compact_control_button(
                "apply-similarity-threshold",
                "Apply",
                enabled,
                cx,
                move |cx| {
                    commit_threshold_editor(get_similarity_settings().threshold, true);
                    cx.notify();
                },
            )),
        modified,
        modified.then(|| {
            setting_reset(
                "reset-similarity-threshold",
                SIMILARITY_THRESHOLD.label,
                cx,
                move |cx| {
                    set_similarity_threshold(default);
                    reset_threshold_editor(default);
                    cx.notify();
                },
            )
        }),
        stacked,
        300.0,
        false,
        enabled,
    )
}

fn render_toggle_row<T: 'static>(
    descriptor: SettingDescriptor,
    help: &'static str,
    (value, default): (bool, bool),
    enabled: bool,
    stacked: bool,
    cx: &mut Context<T>,
    set: fn(bool),
) -> impl IntoElement + use<T> {
    setting_row_with_enabled(
        descriptor.id,
        descriptor.label,
        Some(help),
        toggle_with_enabled(
            descriptor.control_id,
            value,
            enabled,
            cx,
            move |value, cx| {
                set(value);
                cx.notify();
            },
        )
        .aria_label(SharedString::from(descriptor.label)),
        value != default,
        (value != default).then(|| {
            setting_reset(
                format!("reset-{}", descriptor.control_id),
                descriptor.label,
                cx,
                move |cx| {
                    set(default);
                    cx.notify();
                },
            )
        }),
        stacked,
        crate::components::settings::widgets::SettingsMetrics::ROW_CONTROL_WIDTH,
        true,
        enabled,
    )
}

#[allow(clippy::too_many_arguments)]
fn render_number_row<T: 'static>(
    descriptor: SettingDescriptor,
    help: &'static str,
    value: u32,
    default: u32,
    min: u32,
    max: u32,
    unit: &'static str,
    enabled: bool,
    stacked: bool,
    cx: &mut Context<T>,
    set: fn(u32),
) -> impl IntoElement + use<T> {
    let decrement = value.saturating_sub(1).max(min);
    let increment = value.saturating_add(1).min(max);
    setting_row_with_enabled(
        descriptor.id,
        descriptor.label,
        Some(help),
        div()
            .w_full()
            .flex()
            .items_center()
            .justify_end()
            .gap_2()
            .child(compact_control_button(
                format!("{}-decrement", descriptor.control_id),
                "−",
                enabled && value > min,
                cx,
                move |cx| {
                    set(decrement);
                    cx.notify();
                },
            ))
            .child(
                div()
                    .min_w(px(64.0))
                    .text_center()
                    .text_sm()
                    .text_color(crate::theme::colors::text_primary())
                    .child(format!("{value} {unit}")),
            )
            .child(compact_control_button(
                format!("{}-increment", descriptor.control_id),
                "+",
                enabled && value < max,
                cx,
                move |cx| {
                    set(increment);
                    cx.notify();
                },
            )),
        value != default,
        (value != default).then(|| {
            setting_reset(
                format!("reset-{}", descriptor.control_id),
                descriptor.label,
                cx,
                move |cx| {
                    set(default);
                    cx.notify();
                },
            )
        }),
        stacked,
        crate::components::settings::widgets::SettingsMetrics::ROW_CONTROL_WIDTH,
        false,
        enabled,
    )
}

fn compact_control_button<T: 'static>(
    id: impl Into<SharedString>,
    label: &str,
    enabled: bool,
    cx: &mut Context<T>,
    on_click: impl Fn(&mut Context<T>) + 'static,
) -> Stateful<Div> {
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
        .px_2()
        .py_1()
        .min_h(px(28.0))
        .min_w(px(28.0))
        .flex()
        .items_center()
        .justify_center()
        .rounded(crate::theme::tokens::Radii::sm())
        .bg(crate::theme::colors::bg_surface())
        .border_1()
        .border_color(crate::theme::colors::border())
        .cursor_pointer()
        .text_xs()
        .text_color(crate::theme::colors::interactive_foreground())
        .child(label.to_string())
        .on_accessible_activate_if(
            enabled,
            cx.listener(move |_this, _event, _window, cx| {
                on_click(cx);
            }),
        )
}

fn similarity_options_modified(
    settings: &similarity::SimilaritySettings,
    defaults: &similarity::SimilaritySettings,
) -> usize {
    usize::from(setting_float_modified(
        settings.threshold,
        defaults.threshold,
    )) + usize::from(settings.hide_similar != defaults.hide_similar)
        + usize::from(settings.color_similar != defaults.color_similar)
        + usize::from(settings.same_user_only != defaults.same_user_only)
        + usize::from(settings.max_delay_secs != defaults.max_delay_secs)
        + usize::from(settings.max_messages_to_check != defaults.max_messages_to_check)
}

fn threshold_editor_snapshot(threshold: f32) -> EditorSnapshot<SimilarityThresholdEditor> {
    THRESHOLD_EDITOR.sync_and_snapshot(
        |_| SimilarityThresholdEditor {
            text: threshold_text(threshold),
            focused: false,
            dirty: false,
            invalid: false,
        },
        |editor| {
            if editor.dirty || editor.invalid {
                return false;
            }
            let text = threshold_text(threshold);
            if editor.text == text {
                return false;
            }
            editor.text = text;
            true
        },
    )
}

fn update_threshold_editor(threshold: f32, update: impl FnOnce(&mut SimilarityThresholdEditor)) {
    let _ = THRESHOLD_EDITOR.apply_user_edit(
        |_| SimilarityThresholdEditor {
            text: threshold_text(threshold),
            focused: false,
            dirty: false,
            invalid: false,
        },
        update,
    );
}

fn reset_threshold_editor(threshold: f32) {
    THRESHOLD_EDITOR.replace_from_authority(SimilarityThresholdEditor {
        text: threshold_text(threshold),
        focused: false,
        dirty: false,
        invalid: false,
    });
}

fn threshold_input() -> Option<Entity<InputState>> {
    THRESHOLD_INPUT.with(|input| input.borrow().as_ref().map(|entry| entry.input.clone()))
}

fn release_threshold_input() {
    let removed = THRESHOLD_INPUT.with(|input| input.borrow_mut().take());
    drop(removed);
}

fn ensure_threshold_input<T: 'static>(
    threshold: f32,
    window: &mut Window,
    cx: &mut Context<T>,
) -> Entity<InputState> {
    let owner = cx.entity_id();
    let existing = THRESHOLD_INPUT.with(|state| {
        state
            .borrow()
            .as_ref()
            .and_then(|entry| (entry.owner == owner).then(|| entry.input.clone()))
    });
    if let Some(input) = existing {
        return input;
    }
    if threshold_input().is_some() {
        release_threshold_input();
    }

    let seed = threshold_editor_snapshot(threshold).draft.text;
    let input = cx.new(|cx| {
        InputState::new(window, cx)
            .with_placeholder("0 to 1")
            .with_value(seed)
    });
    let subscription = cx.subscribe_in(
        &input,
        window,
        move |_this, input, event: &InputEvent, window, cx| {
            let threshold = get_similarity_settings().threshold;
            match event {
                InputEvent::Focus => {
                    update_threshold_editor(threshold, |editor| editor.focused = true);
                    cx.notify();
                }
                InputEvent::Change => {
                    let value = input.read(cx).value().to_string();
                    let limited = limit_threshold_text(&value);
                    if value != limited {
                        input.update(cx, |input, cx| input.set_value(limited, window, cx));
                        return;
                    }
                    update_threshold_editor(threshold, |editor| {
                        editor.text = value;
                        editor.dirty = threshold_draft_is_dirty(&editor.text, threshold);
                        editor.invalid = false;
                    });
                    cx.notify();
                }
                InputEvent::PressEnter => {
                    commit_threshold_editor(threshold, true);
                    sync_threshold_input_text(
                        get_similarity_settings().threshold,
                        true,
                        window,
                        cx,
                    );
                    cx.notify();
                }
                InputEvent::Blur => {
                    commit_threshold_editor(threshold, false);
                    update_threshold_editor(get_similarity_settings().threshold, |editor| {
                        editor.focused = false;
                    });
                    sync_threshold_input_text(
                        get_similarity_settings().threshold,
                        false,
                        window,
                        cx,
                    );
                    cx.notify();
                }
            }
        },
    );
    THRESHOLD_INPUT.with(|state| {
        *state.borrow_mut() = Some(SimilarityThresholdInput {
            owner,
            input: input.clone(),
            _subscription: subscription,
        });
    });
    input
}

fn sync_threshold_input_text<T: 'static>(
    threshold: f32,
    force: bool,
    window: &mut Window,
    cx: &mut Context<T>,
) {
    let Some(input) = threshold_input() else {
        return;
    };
    let snapshot = threshold_editor_snapshot(threshold);
    if !force && (snapshot.draft.dirty || snapshot.draft.invalid) {
        return;
    }
    let text = snapshot.draft.text;
    if input.read(cx).value() == text {
        return;
    }
    input.update(cx, |input, cx| input.set_value(text, window, cx));
}

fn commit_threshold_editor(threshold: f32, preserve_focus: bool) {
    let snapshot = threshold_editor_snapshot(threshold);
    if !snapshot.draft.dirty {
        return;
    }
    let Some(value) = parse_threshold_text(&snapshot.draft.text) else {
        update_threshold_editor(threshold, |editor| editor.invalid = true);
        return;
    };
    set_similarity_threshold(value);
    let authority = get_similarity_settings().threshold;
    let was_focused = snapshot.draft.focused && preserve_focus;
    reset_threshold_editor(authority);
    if was_focused {
        update_threshold_editor(authority, |editor| editor.focused = true);
    }
}

fn threshold_summary(editor: &SimilarityThresholdEditor, needs_review: bool) -> String {
    if needs_review {
        "Needs review · Threshold, display, and lookback".to_string()
    } else if editor.dirty {
        "Unsaved draft · Threshold, display, and lookback".to_string()
    } else {
        "Threshold, display, and lookback".to_string()
    }
}

fn threshold_text(threshold: f32) -> String {
    if threshold == 0.0 {
        "0".to_string()
    } else {
        let text = threshold.to_string();
        if text.len() <= 32 {
            text
        } else {
            format!("{threshold:.8e}")
        }
    }
}

fn threshold_draft_is_dirty(text: &str, threshold: f32) -> bool {
    text != threshold_text(threshold)
}

fn parse_threshold_text(text: &str) -> Option<f32> {
    let value = text.trim().parse::<f32>().ok()?;
    (value.is_finite() && (0.0..=1.0).contains(&value)).then_some(if value == 0.0 {
        0.0
    } else {
        value
    })
}

fn limit_threshold_text(text: &str) -> String {
    text.chars().take(32).collect()
}

pub(crate) fn reset_similarity_editor_after_settings_replace() {
    reset_threshold_editor(get_similarity_settings().threshold);
}

fn percent_text(threshold: f32) -> String {
    let mut text = format!("{:.6}", threshold * 100.0);
    while text.ends_with('0') {
        text.pop();
    }
    if text.ends_with('.') {
        text.pop();
    }
    format!("{text}%")
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test;

    #[test]
    fn threshold_text_keeps_the_stored_f32_editable() {
        let threshold = 0.913742_f32;
        assert_eq!(threshold_text(threshold).parse::<f32>().unwrap(), threshold);
        assert_eq!(threshold_text(-0.0), "0");
        for threshold in [f32::MIN_POSITIVE, f32::MAX, 1.0e-30] {
            let text = threshold_text(threshold);
            assert!(text.len() <= 32);
            assert_eq!(text.parse::<f32>().unwrap(), threshold);
        }
        assert!(percent_text(threshold).starts_with("91.374"));
    }

    #[test]
    fn modified_tolerance_matches_the_shared_setting_rule() {
        assert!(!setting_float_modified(0.9, 0.90005));
        assert!(setting_float_modified(0.9, 0.9002));
    }

    #[test]
    fn threshold_rejects_nonfinite_and_out_of_range_drafts() {
        assert_eq!(parse_threshold_text("0.913742"), Some(0.913742));
        assert_eq!(parse_threshold_text("-0"), Some(0.0));
        for invalid in ["", "NaN", "inf", "-0.1", "1.1"] {
            assert_eq!(parse_threshold_text(invalid), None, "{invalid}");
        }
    }

    #[test]
    fn threshold_input_limit_keeps_utf8_boundaries() {
        let value = format!("{}x", "é".repeat(32));
        let limited = limit_threshold_text(&value);
        assert_eq!(limited.chars().count(), 32);
        assert!(limited.is_char_boundary(limited.len()));
    }

    #[test]
    fn untouched_threshold_draft_does_not_commit() {
        let threshold = 0.913742_f32;
        assert!(!threshold_draft_is_dirty(
            &threshold_text(threshold),
            threshold
        ));
        assert!(threshold_draft_is_dirty("0.9", threshold));
    }
}
