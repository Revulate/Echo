//! Chat › Timestamps: the token builder that edits the message timestamp
//! pattern, and its transient editing state.

use super::*;

/// Transient, settings-only state for the timestamp token builder.
///
/// Global for the same reason as [`PLAYER_PATHS_EDITOR`]: the section
/// renderers are free functions shared by the settings modal and the native
/// popout, so there is no per-surface entity to hang editing state on. Only
/// editing affordances live here; the pattern itself is persisted settings.
#[derive(Default)]
struct TimestampBuilderState {
    /// Chip whose property editor is expanded.
    selected: Option<usize>,
    /// Whether the grouped "Add a piece" menu is open.
    add_menu_open: bool,
    /// Whether the custom-separator field owns settings text input.
    literal_focused: bool,
    /// Draft text for the custom separator.
    literal_draft: String,
    /// Last add/reorder/validation message.
    status: Option<String>,
    status_is_error: bool,
    /// Whether a preview refresh timer is already in flight.
    preview_pending: bool,
    /// Formatter generation the in-flight timer was armed for.
    preview_generation: u64,
}

static TIMESTAMP_BUILDER: LazyLock<Mutex<TimestampBuilderState>> =
    LazyLock::new(|| Mutex::new(TimestampBuilderState::default()));

fn recovered_timestamp_builder_state() -> TimestampBuilderState {
    TimestampBuilderState {
        status: Some(
            "Timestamp builder state was reset after an internal editor error. Select or edit a piece again."
                .to_string(),
        ),
        status_is_error: true,
        ..TimestampBuilderState::default()
    }
}

fn lock_timestamp_builder_state(
    mutex: &Mutex<TimestampBuilderState>,
) -> (MutexGuard<'_, TimestampBuilderState>, bool) {
    match mutex.lock() {
        Ok(state) => (state, false),
        Err(poisoned) => {
            let mut state = poisoned.into_inner();
            *state = recovered_timestamp_builder_state();
            mutex.clear_poison();
            crate::log_ui!(warn, "Reset poisoned timestamp builder state");
            (state, true)
        }
    }
}

fn update_timestamp_builder(update: impl FnOnce(&mut TimestampBuilderState)) -> bool {
    let (mut state, recovered) = lock_timestamp_builder_state(&TIMESTAMP_BUILDER);
    if recovered {
        return false;
    }
    update(&mut state);
    true
}

fn apply_timestamp_builder_authoritative(update: impl FnOnce(&mut TimestampBuilderState)) {
    let (mut state, _) = lock_timestamp_builder_state(&TIMESTAMP_BUILDER);
    update(&mut state);
}

pub(crate) fn reset_timestamp_builder_after_settings_replace() {
    let (mut state, _) = lock_timestamp_builder_state(&TIMESTAMP_BUILDER);
    *state = TimestampBuilderState {
        status: Some("Timestamp builder reset after settings were replaced".to_string()),
        ..TimestampBuilderState::default()
    };
}

/// One render pass's view of the builder's transient state.
#[derive(Default)]
pub(super) struct TimestampBuilderView {
    pub(super) selected: Option<usize>,
    pub(super) add_menu_open: bool,
    pub(super) literal_focused: bool,
    pub(super) literal_draft: String,
    pub(super) status: Option<String>,
    pub(super) status_is_error: bool,
}

/// Snapshot the builder's transient state, releasing the lock before render.
pub(super) fn timestamp_builder_snapshot() -> TimestampBuilderView {
    let (state, _) = lock_timestamp_builder_state(&TIMESTAMP_BUILDER);
    TimestampBuilderView {
        selected: state.selected,
        add_menu_open: state.add_menu_open,
        literal_focused: state.literal_focused,
        literal_draft: state.literal_draft.clone(),
        status: state.status.clone(),
        status_is_error: state.status_is_error,
    }
}

/// Persist an edited token list, keeping the builder's selection in sync.
///
/// Validation lives in the backend so the settings UI can never persist a
/// pattern the compiled formatter would have to guess about; a rejection is
/// surfaced as inline status text instead of silently doing nothing.
pub(super) fn commit_timestamp_tokens<T: 'static>(
    tokens: Vec<TimestampToken>,
    select: Option<usize>,
    cx: &mut Context<T>,
) {
    let pattern = TimestampPattern {
        version: TIMESTAMP_PATTERN_VERSION,
        tokens,
    };
    match crate::backend::settings::set_message_timestamp_pattern(pattern) {
        Ok(()) => {
            apply_timestamp_builder_authoritative(|state| {
                state.selected = select;
                state.status = None;
                state.status_is_error = false;
            });
        }
        Err(err) => {
            apply_timestamp_builder_authoritative(|state| {
                state.status = Some(err.message().to_string());
                state.status_is_error = true;
            });
        }
    }
    cx.notify();
}

/// Keep the live preview fresh without adding a per-frame wakeup.
///
/// Exactly one timer may be in flight, and it is only re-armed while the
/// builder is actually rendering. Closing Settings or hiding timestamps
/// therefore stops the loop after at most one more tick. Millisecond patterns
/// are capped at 10 Hz by [`TimestampResolution::preview_interval_ms`].
fn arm_timestamp_preview_refresh<T: 'static>(
    resolution: TimestampResolution,
    generation: u64,
    cx: &mut Context<T>,
) {
    let should_arm = {
        let (mut state, _) = lock_timestamp_builder_state(&TIMESTAMP_BUILDER);
        mark_timestamp_preview_pending(&mut state, generation)
    };
    if !should_arm {
        return;
    }

    let interval = std::time::Duration::from_millis(resolution.preview_interval_ms());
    cx.spawn(async move |this, cx| {
        cx.background_executor().timer(interval).await;
        finish_timestamp_preview_timer(generation);
        let _ = this.update(cx, |_this, cx| {
            crate::backend::metrics::record_ui_notify_source("settings.timestamp.preview");
            cx.notify();
        });
    })
    .detach();
}

fn mark_timestamp_preview_pending(state: &mut TimestampBuilderState, generation: u64) -> bool {
    if state.preview_pending && state.preview_generation == generation {
        return false;
    }
    state.preview_pending = true;
    state.preview_generation = generation;
    true
}

fn finish_timestamp_preview_timer(generation: u64) {
    let (mut state, recovered) = lock_timestamp_builder_state(&TIMESTAMP_BUILDER);
    if !recovered {
        finish_timestamp_preview_timer_state(&mut state, generation);
    }
}

fn finish_timestamp_preview_timer_state(state: &mut TimestampBuilderState, generation: u64) {
    if state.preview_pending && state.preview_generation == generation {
        state.preview_pending = false;
    }
}

#[cfg(test)]
mod timestamp_builder_state_tests {
    use super::*;
    use core::prelude::v1::test;

    #[test]
    fn recovery_discards_transient_builder_state() {
        let recovered = recovered_timestamp_builder_state();

        assert!(recovered.selected.is_none());
        assert!(!recovered.add_menu_open);
        assert!(!recovered.literal_focused);
        assert!(recovered.literal_draft.is_empty());
        assert!(recovered.status_is_error);
        assert!(recovered.status.is_some());
        assert!(!recovered.preview_pending);
        assert_eq!(recovered.preview_generation, 0);
    }

    #[test]
    fn preview_timer_is_single_flight_per_generation() {
        let mut state = TimestampBuilderState::default();

        assert!(mark_timestamp_preview_pending(&mut state, 7));
        assert!(!mark_timestamp_preview_pending(&mut state, 7));
        assert!(mark_timestamp_preview_pending(&mut state, 8));
        assert!(state.preview_pending);
        assert_eq!(state.preview_generation, 8);
    }

    #[test]
    fn prior_generation_timer_cannot_finish_current_timer() {
        let mut state = TimestampBuilderState {
            preview_pending: true,
            preview_generation: 8,
            ..TimestampBuilderState::default()
        };

        finish_timestamp_preview_timer_state(&mut state, 7);
        assert!(state.preview_pending);

        finish_timestamp_preview_timer_state(&mut state, 8);
        assert!(!state.preview_pending);
    }
}

/// Route key input to the timestamp builder. Returns whether it consumed one.
///
/// Left/Right reorder the selected chip and Delete removes it, matching the
/// pointer affordances on the chip itself. Keys typed in the custom-separator
/// field belong to that text input.
pub(super) fn handle_timestamp_builder_key<T: 'static>(
    event: &KeyDownEvent,
    cx: &mut Context<T>,
) -> bool {
    let view = timestamp_builder_snapshot();

    if view.literal_focused {
        return false;
    }

    if view.add_menu_open && event.keystroke.key == "escape" {
        update_timestamp_builder(|state| {
            state.add_menu_open = false;
            state.literal_focused = false;
        });
        return true;
    }

    let Some(index) = view.selected else {
        return false;
    };
    let pattern = crate::backend::settings::get_message_timestamp_pattern();
    if index >= pattern.tokens.len() {
        update_timestamp_builder(|state| state.selected = None);
        return true;
    }

    match event.keystroke.key.as_str() {
        "left" if index > 0 => {
            let mut tokens = pattern.tokens;
            tokens.swap(index, index - 1);
            commit_timestamp_tokens(tokens, Some(index - 1), cx);
            true
        }
        "right" if index + 1 < pattern.tokens.len() => {
            let mut tokens = pattern.tokens;
            tokens.swap(index, index + 1);
            commit_timestamp_tokens(tokens, Some(index + 1), cx);
            true
        }
        "delete" | "backspace" if pattern.tokens.len() > 1 => {
            let mut tokens = pattern.tokens;
            tokens.remove(index);
            commit_timestamp_tokens(tokens, None, cx);
            true
        }
        "escape" => {
            update_timestamp_builder(|state| state.selected = None);
            true
        }
        _ => false,
    }
}

/// Append draft text, enforcing the visible-character limit up front so the
/// field can never hold something the backend would reject on submit.
fn push_timestamp_literal_draft(state: &mut TimestampBuilderState, text: &str) {
    for ch in text.chars() {
        if state.literal_draft.chars().count() >= MAX_TIMESTAMP_LITERAL_CHARS {
            break;
        }
        state.literal_draft.push(ch);
    }
    state.status = None;
    state.status_is_error = false;
}

pub(super) fn render_timestamp_builder<T: 'static>(
    pattern: TimestampPattern,
    literal_preset_select: SettingsSelectState,
    literal_input: DraftField<T>,
    cx: &mut Context<T>,
) -> impl IntoElement + use<T> {
    let formatter = crate::backend::settings::message_timestamp_formatter();
    let preview = crate::utils::timestamp::format_message_timestamp_with_formatter(
        chrono::Local::now().timestamp_millis(),
        formatter.clone(),
    )
    .unwrap_or_else(|| "--:--".to_string());
    arm_timestamp_preview_refresh(formatter.resolution, formatter.generation, cx);

    let TimestampBuilderView {
        selected,
        add_menu_open,
        literal_draft,
        status,
        status_is_error,
        ..
    } = timestamp_builder_snapshot();
    let selected = selected.filter(|index| *index < pattern.tokens.len());

    div()
        .w_full()
        .flex()
        .flex_col()
        .gap_2()
        .child(render_timestamp_preview(&preview))
        .child(render_timestamp_chips(&pattern, selected, cx))
        .when_some(selected, |el, index| {
            el.child(render_timestamp_token_properties(
                &pattern,
                index,
                &literal_preset_select,
                cx,
            ))
        })
        .child(render_timestamp_add_menu(
            &pattern,
            add_menu_open,
            &literal_draft,
            literal_input,
            cx,
        ))
        .when_some(status, |el, message| {
            el.child(
                div()
                    .text_xs()
                    .text_color(if status_is_error {
                        colors::error()
                    } else {
                        colors::text_muted()
                    })
                    .child(message),
            )
        })
        .child(
            div()
                .flex()
                .child(render_button("Reset timestamp format", false, cx, |cx| {
                    commit_timestamp_tokens(TimestampPattern::default().tokens, None, cx);
                })),
        )
}

fn render_timestamp_preview(preview: &str) -> impl IntoElement + use<> {
    div()
        .w_full()
        .flex()
        .items_center()
        .gap_2()
        .child(
            div()
                .text_xs()
                .text_color(colors::text_muted())
                .child("Preview"),
        )
        .child(
            div()
                .px_2()
                .py_1()
                .rounded(Radii::sm())
                .bg(colors::bg_surface())
                .border_1()
                .border_color(colors::border())
                .text_sm()
                .font_weight(FontWeight::MEDIUM)
                .text_color(colors::text_primary())
                .child(preview.to_string()),
        )
}

/// Ordered chips with per-chip reorder and remove affordances.
fn render_timestamp_chips<T: 'static>(
    pattern: &TimestampPattern,
    selected: Option<usize>,
    cx: &mut Context<T>,
) -> impl IntoElement + use<T> {
    let tokens = pattern.tokens.clone();
    let count = tokens.len();
    let names: Vec<String> = tokens.iter().map(TimestampToken::accessible_name).collect();

    div()
        .w_full()
        .flex()
        .flex_wrap()
        .items_center()
        .gap_1()
        .children(tokens.iter().enumerate().map(|(index, token)| {
            let is_selected = selected == Some(index);
            let tokens_for_left = tokens.clone();
            let tokens_for_right = tokens.clone();
            let tokens_for_remove = tokens.clone();
            let move_left = (index > 0).then(|| {
                render_timestamp_chip_action(
                    format!("timestamp-chip-left-{index}"),
                    "\u{25c0}",
                    format!(
                        "Move {} before {}",
                        names[index],
                        names[index.saturating_sub(1)]
                    ),
                    cx,
                    move |cx| {
                        let mut tokens = tokens_for_left.clone();
                        tokens.swap(index, index - 1);
                        commit_timestamp_tokens(tokens, Some(index - 1), cx);
                    },
                )
            });
            let move_right = (index + 1 < count).then(|| {
                render_timestamp_chip_action(
                    format!("timestamp-chip-right-{index}"),
                    "\u{25b6}",
                    format!("Move {} after {}", names[index], names[index + 1]),
                    cx,
                    move |cx| {
                        let mut tokens = tokens_for_right.clone();
                        tokens.swap(index, index + 1);
                        commit_timestamp_tokens(tokens, Some(index + 1), cx);
                    },
                )
            });
            let remove = (count > 1).then(|| {
                render_timestamp_chip_action(
                    format!("timestamp-chip-remove-{index}"),
                    "\u{2715}",
                    format!("Remove {}", names[index]),
                    cx,
                    move |cx| {
                        let mut tokens = tokens_for_remove.clone();
                        tokens.remove(index);
                        commit_timestamp_tokens(tokens, None, cx);
                    },
                )
            });

            div()
                .id(SharedString::from(format!("timestamp-chip-{index}")))
                .role(Role::Button)
                .map(|control| {
                    crate::components::settings::widgets::settings_keyboard_control(control, cx)
                })
                .a11y_synthetic_children(crate::components::ui::accessibility::apply_description)
                .aria_label(SharedString::from(names[index].clone()))
                .aria_selected(is_selected)
                .flex()
                .items_center()
                .gap_1()
                .pl_2()
                .pr_1()
                .py_0p5()
                .min_h(px(28.0))
                .rounded(Radii::sm())
                .bg(if is_selected {
                    colors::bg_hover()
                } else {
                    colors::bg_surface()
                })
                .border_1()
                .border_color(if is_selected {
                    colors::accent()
                } else {
                    colors::border()
                })
                .cursor_pointer()
                .on_activate(cx.listener(move |_this, _event, _window, cx| {
                    update_timestamp_builder(|state| {
                        state.selected = if state.selected == Some(index) {
                            None
                        } else {
                            Some(index)
                        };
                    });
                    cx.notify();
                }))
                .on_a11y_action(AccessibleAction::Click, {
                    let entity = cx.entity();
                    move |_, _, app| {
                        entity.update(app, |_, cx| {
                            update_timestamp_builder(|state| {
                                state.selected = if state.selected == Some(index) {
                                    None
                                } else {
                                    Some(index)
                                };
                            });
                            cx.notify();
                        })
                    }
                })
                .child(
                    div()
                        .text_xs()
                        .text_color(colors::text_primary())
                        .child(token.chip_label()),
                )
                .when_some(move_left, |el, action| el.child(action))
                .when_some(move_right, |el, action| el.child(action))
                .when_some(remove, |el, action| el.child(action))
        }))
}

/// One 20px glyph button inside a chip, carrying its own accessible label.
fn render_timestamp_chip_action<T: 'static, F: Fn(&mut Context<T>) + 'static>(
    id: String,
    glyph: &'static str,
    label: String,
    cx: &mut Context<T>,
    on_click: F,
) -> impl IntoElement + use<T, F> {
    let on_click = std::rc::Rc::new(on_click);
    let pointer = on_click.clone();
    let entity = cx.entity();
    div()
        .id(SharedString::from(id))
        .role(Role::Button)
        .map(|control| crate::components::settings::widgets::settings_keyboard_control(control, cx))
        .a11y_synthetic_children(crate::components::ui::accessibility::apply_description)
        .aria_label(SharedString::from(label))
        .flex()
        .items_center()
        .justify_center()
        .w(px(20.0))
        .h(px(20.0))
        .rounded(Radii::sm())
        .cursor_pointer()
        .text_xs()
        .text_color(colors::text_muted())
        .hover(|s| {
            s.bg(colors::hover_surface())
                .text_color(colors::text_primary())
        })
        .on_activate(cx.listener(move |_this, _event, _window, cx| {
            cx.stop_propagation();
            pointer(cx);
        }))
        .on_a11y_action(AccessibleAction::Click, move |_, _, app| {
            entity.update(app, |_, cx| on_click(cx));
        })
        .child(glyph)
}

/// Property editor for the selected chip.
fn render_timestamp_token_properties<T: 'static>(
    pattern: &TimestampPattern,
    index: usize,
    literal_preset_select: &SettingsSelectState,
    cx: &mut Context<T>,
) -> impl IntoElement + use<T> {
    let tokens = pattern.tokens.clone();
    let token = tokens[index].clone();

    let body = match token {
        TimestampToken::Hour24 { padded } | TimestampToken::Hour12 { padded } => {
            let twelve_hour = matches!(tokens[index], TimestampToken::Hour12 { .. });
            let tokens_for_change = tokens.clone();
            select_buttons(
                "timestamp-hour-padding",
                &[
                    SelectOption::new("compact", "9"),
                    SelectOption::new("padded", "09"),
                ],
                if padded { "padded" } else { "compact" },
                cx,
                move |value, cx| {
                    let mut tokens = tokens_for_change.clone();
                    let padded = value == "padded";
                    tokens[index] = if twelve_hour {
                        TimestampToken::Hour12 { padded }
                    } else {
                        TimestampToken::Hour24 { padded }
                    };
                    commit_timestamp_tokens(tokens, Some(index), cx);
                },
            )
            .into_any_element()
        }
        TimestampToken::Meridiem { uppercase } => {
            let tokens_for_change = tokens.clone();
            select_buttons(
                "timestamp-meridiem-case",
                &[
                    SelectOption::new("lower", "am/pm"),
                    SelectOption::new("upper", "AM/PM"),
                ],
                if uppercase { "upper" } else { "lower" },
                cx,
                move |value, cx| {
                    let mut tokens = tokens_for_change.clone();
                    tokens[index] = TimestampToken::Meridiem {
                        uppercase: value == "upper",
                    };
                    commit_timestamp_tokens(tokens, Some(index), cx);
                },
            )
            .into_any_element()
        }
        TimestampToken::Fraction { digits } => {
            let tokens_for_change = tokens.clone();
            select_buttons(
                "timestamp-fraction-digits",
                &[
                    SelectOption::new("1", "1 digit"),
                    SelectOption::new("2", "2 digits"),
                    SelectOption::new("3", "3 digits"),
                ],
                &digits.to_string(),
                cx,
                move |value, cx| {
                    let mut tokens = tokens_for_change.clone();
                    tokens[index] = TimestampToken::Fraction {
                        digits: value.parse().unwrap_or(3),
                    };
                    commit_timestamp_tokens(tokens, Some(index), cx);
                },
            )
            .into_any_element()
        }
        TimestampToken::Literal { ref text } => {
            let tokens_for_change = tokens.clone();
            let options: Vec<SelectOption> = TIMESTAMP_LITERAL_PRESETS
                .iter()
                .map(|(value, label)| SelectOption::new(*value, *label))
                .collect();
            let current = text.clone();
            div()
                .on_key_down(cx.listener(|_this, event: &KeyDownEvent, _window, cx| {
                    if matches!(
                        event.keystroke.key.as_str(),
                        "left" | "right" | "backspace" | "delete"
                    ) {
                        cx.stop_propagation();
                    }
                }))
                .child(settings_select(
                    "timestamp-literal-preset",
                    "Separator preset",
                    &options,
                    &current,
                    literal_preset_select,
                    cx,
                    move |value, cx| {
                        let mut tokens = tokens_for_change.clone();
                        tokens[index] = TimestampToken::literal(&value);
                        commit_timestamp_tokens(tokens, Some(index), cx);
                    },
                ))
                .into_any_element()
        }
        TimestampToken::Minute | TimestampToken::Second => div()
            .text_xs()
            .text_color(colors::text_muted())
            .child("This piece has no options.")
            .into_any_element(),
    };

    div()
        .w_full()
        .flex()
        .flex_col()
        .gap_1()
        .p_2()
        .rounded(Radii::sm())
        .bg(colors::bg_surface())
        .border_1()
        .border_color(colors::border())
        .child(
            div()
                .text_xs()
                .text_color(colors::text_muted())
                .child(format!("{} options", tokens[index].accessible_name())),
        )
        .child(body)
}

/// Grouped add menu. Entries that would produce an invalid pattern are
/// rendered disabled with their reason, so the menu can never offer an
/// addition the backend would reject.
fn render_timestamp_add_menu<T: 'static>(
    pattern: &TimestampPattern,
    is_open: bool,
    literal_draft: &str,
    literal_input: DraftField<T>,
    cx: &mut Context<T>,
) -> impl IntoElement + use<T> {
    let time_pieces: Vec<TimestampToken> = vec![
        TimestampToken::Hour24 { padded: false },
        TimestampToken::Hour12 { padded: false },
        TimestampToken::Minute,
        TimestampToken::Second,
        TimestampToken::Meridiem { uppercase: false },
    ];
    let separators: Vec<TimestampToken> = TIMESTAMP_LITERAL_PRESETS
        .iter()
        .map(|(value, _)| TimestampToken::literal(value))
        .collect();
    let separator_labels: Vec<&str> = TIMESTAMP_LITERAL_PRESETS
        .iter()
        .map(|(_, label)| *label)
        .collect();
    let advanced: Vec<TimestampToken> = vec![
        TimestampToken::Fraction { digits: 1 },
        TimestampToken::Fraction { digits: 2 },
        TimestampToken::Fraction { digits: 3 },
    ];
    let advanced_labels = ["Fraction 1 digit", "Fraction 2 digits", "Fraction 3 digits"];
    let draft = literal_draft.to_string();
    let draft_for_add = draft.clone();

    div()
        .w_full()
        .flex()
        .flex_col()
        .gap_2()
        .child(
            div()
                .id("timestamp-add-menu-toggle")
                .role(Role::Button)
                .map(|control| {
                    crate::components::settings::widgets::settings_keyboard_control(control, cx)
                })
                .a11y_synthetic_children(crate::components::ui::accessibility::apply_description)
                .aria_label("Add a timestamp piece")
                .aria_expanded(is_open)
                .flex()
                .items_center()
                .justify_center()
                .self_start()
                .px_2()
                .min_h(px(28.0))
                .rounded(Radii::sm())
                .bg(colors::bg_surface())
                .border_1()
                .border_color(colors::border())
                .cursor_pointer()
                .hover(|s| s.bg(colors::bg_hover()))
                .text_xs()
                .text_color(colors::interactive_foreground())
                .tab_index(0)
                .on_accessible_activate(cx.listener(move |_this, _event, _window, cx| {
                    update_timestamp_builder(|state| {
                        state.add_menu_open = !state.add_menu_open;
                        if !state.add_menu_open {
                            state.literal_focused = false;
                        }
                    });
                    cx.notify();
                }))
                .child(if is_open { "Close" } else { "Add a piece" }),
        )
        .when(is_open, |el| {
            el.child(
                menu_panel()
                    .w_full()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .p_2()
                    .child(render_timestamp_add_group(
                        "Time pieces",
                        pattern,
                        &time_pieces,
                        &time_pieces
                            .iter()
                            .map(|token| token.kind().label().to_string())
                            .collect::<Vec<_>>(),
                        "time",
                        cx,
                    ))
                    .child(render_timestamp_add_group(
                        "Separators",
                        pattern,
                        &separators,
                        &separator_labels
                            .iter()
                            .map(|label| (*label).to_string())
                            .collect::<Vec<_>>(),
                        "separator",
                        cx,
                    ))
                    .child(render_timestamp_add_group(
                        "Advanced",
                        pattern,
                        &advanced,
                        &advanced_labels
                            .iter()
                            .map(|label| (*label).to_string())
                            .collect::<Vec<_>>(),
                        "advanced",
                        cx,
                    ))
                    .child(
                        div()
                            .w_full()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(
                                div()
                                    .text_xs()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(colors::text_secondary())
                                    .child("Custom separator"),
                            )
                            .child(literal_input.render(cx))
                            .child(
                                div()
                                    .w_full()
                                    .min_w_0()
                                    .flex()
                                    .flex_wrap()
                                    .gap_1()
                                    .child(render_button("Paste separator", false, cx, |cx| {
                                        let pasted =
                                            crate::utils::read_text(cx).unwrap_or_default();
                                        update_timestamp_builder(|state| {
                                            state.literal_draft.clear();
                                            push_timestamp_literal_draft(state, &pasted);
                                        });
                                        cx.notify();
                                    }))
                                    .child(render_button("Add separator", false, cx, move |cx| {
                                        add_custom_timestamp_literal(&draft_for_add, cx);
                                    })),
                            ),
                    ),
            )
        })
}

/// One labeled group of add-menu entries.
fn render_timestamp_add_group<T: 'static>(
    title: &str,
    pattern: &TimestampPattern,
    tokens: &[TimestampToken],
    labels: &[String],
    id_prefix: &str,
    cx: &mut Context<T>,
) -> impl IntoElement + use<T> {
    let entries: Vec<(TimestampToken, String, Option<&'static str>)> = tokens
        .iter()
        .enumerate()
        .map(|(position, token)| {
            let reason = pattern.can_append(token).err().map(|err| err.message());
            (
                token.clone(),
                labels
                    .get(position)
                    .cloned()
                    .unwrap_or_else(|| token.kind().label().to_string()),
                reason,
            )
        })
        .collect();
    let prefix = id_prefix.to_string();

    div()
        .w_full()
        .flex()
        .flex_col()
        .gap_1()
        .child(
            div()
                .text_xs()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(colors::text_secondary())
                .child(title.to_string()),
        )
        .child(
            div()
                .flex()
                .flex_wrap()
                .gap_1()
                .children(entries.into_iter().enumerate().map(
                    |(position, (token, label, reason))| {
                        let disabled = reason.is_some();
                        let mut element = div()
                            .id(SharedString::from(format!(
                                "timestamp-add-{prefix}-{position}"
                            )))
                            .role(Role::Button)
                            .map(|control| {
                                crate::components::settings::widgets::settings_keyboard_control_if_enabled(
                                    control, !disabled, cx,
                                )
                            })
                            .a11y_synthetic_children(
                                crate::components::ui::accessibility::apply_description,
                            )
                            .aria_label(SharedString::from(format!("Add {label}")))
                            .flex()
                            .items_center()
                            .px_2()
                            .min_h(px(28.0))
                            .rounded(Radii::sm())
                            .bg(colors::bg_elevated())
                            .border_1()
                            .border_color(colors::border())
                            .text_xs()
                            .text_color(if disabled {
                                colors::text_muted()
                            } else {
                                colors::interactive_foreground()
                            })
                            .child(label);

                        if let Some(reason) = reason {
                            element = element
                                .aria_description(SharedString::from(reason))
                                .disabled_button();
                        } else {
                            element = element
                                .cursor_pointer()
                                .hover(button_hover)
                                .on_accessible_activate(cx.listener(
                                    move |_this, _event, _window, cx| {
                                        let mut tokens =
                                        crate::backend::settings::get_message_timestamp_pattern()
                                            .tokens;
                                        tokens.push(token.clone());
                                        let select = tokens.len() - 1;
                                        commit_timestamp_tokens(tokens, Some(select), cx);
                                    },
                                ));
                        }
                        element
                    },
                )),
        )
}

/// Validate and append a custom separator from the builder's draft field.
pub(super) fn timestamp_literal_input<T: 'static>(draft: &str) -> DraftInput<T> {
    DraftInput::new(
        "timestamp-custom-literal",
        draft,
        "Type or paste up to 8 characters",
    )
    .label("Custom separator")
    .on_focus(|_cx| {
        update_timestamp_builder(|state| state.literal_focused = true);
    })
    .on_blur(|_cx| {
        update_timestamp_builder(|state| state.literal_focused = false);
    })
    .on_change(|text, _cx| {
        update_timestamp_builder(|state| {
            state.literal_draft.clear();
            push_timestamp_literal_draft(state, &text);
        });
    })
    .on_enter(|cx| {
        let draft = timestamp_builder_snapshot().literal_draft;
        add_custom_timestamp_literal(&draft, cx);
    })
    .on_escape(|_window, _cx| {
        update_timestamp_builder(|state| state.literal_draft.clear());
    })
}

fn add_custom_timestamp_literal<T: 'static>(draft: &str, cx: &mut Context<T>) {
    if let Err(err) = validate_timestamp_literal(draft) {
        update_timestamp_builder(|state| {
            state.status = Some(err.message().to_string());
            state.status_is_error = true;
        });
        cx.notify();
        return;
    }

    let mut tokens = crate::backend::settings::get_message_timestamp_pattern().tokens;
    tokens.push(TimestampToken::literal(draft));
    let select = tokens.len() - 1;
    commit_timestamp_tokens(tokens, Some(select), cx);
    update_timestamp_builder(|state| {
        state.literal_draft.clear();
        state.literal_focused = false;
    });
}
