//! Input element - Visual rendering for InputState
//!
//! Renders the text input with cursor, selection, and spell check underlines.

use gpui::{
    Action, App, Bounds, CaretAffinity, ContentMask, Context, ElementInputHandler, Entity,
    FocusHandle, Hsla, InteractiveElement, IntoElement, MouseButton, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, ParentElement, Pixels, Point, RenderOnce, Role, ShapedLine,
    StatefulInteractiveElement, Styled, TextAlign, TextRun, Window, accesskit, canvas, div, fill,
    point, px, size,
};
use std::ops::Range;

use crate::theme::colors;
use crate::theme::tokens::{Radii, RadiiPixels, Spacing};

use super::CONTEXT;
use super::bidi_probe;
use super::state::InputState;

const CARET_WIDTH: Pixels = px(2.0);

/// State passed from prepaint to paint phase
struct PrepaintState {
    bounds: Bounds<Pixels>,
    text_origin: Point<Pixels>,
    line_height_px: Pixels,
    line: ShapedLine,
    selection_bounds: Vec<Bounds<Pixels>>,
    marked_underline_bounds: Vec<Bounds<Pixels>>,
    spell_underline_bounds: Vec<Bounds<Pixels>>,
    cursor_bounds: Option<Bounds<Pixels>>,
    focus_handle: FocusHandle,
    state_entity: Entity<InputState>,
}

/// The visual Input element
#[derive(IntoElement)]
pub struct Input {
    state: Entity<InputState>,
    appearance: bool,
    bordered: bool,
    focus_bordered: bool,
    disabled: bool,
    accessibility_label: Option<String>,
    accessibility_secure: bool,
    accessibility_search: bool,
}

impl Input {
    /// Create a new Input element bound to an InputState
    pub fn new(state: &Entity<InputState>) -> Self {
        Self {
            state: state.clone(),
            appearance: true,
            bordered: true,
            focus_bordered: true,
            disabled: false,
            accessibility_label: None,
            accessibility_secure: false,
            accessibility_search: false,
        }
    }

    /// Enable or disable built-in input background chrome.
    pub fn appearance(mut self, appearance: bool) -> Self {
        self.appearance = appearance;
        self
    }

    /// Enable or disable the default border.
    pub fn bordered(mut self, bordered: bool) -> Self {
        self.bordered = bordered;
        self
    }

    /// Enable or disable accent border when focused.
    pub fn focus_bordered(mut self, focus_bordered: bool) -> Self {
        self.focus_bordered = focus_bordered;
        self
    }

    /// Override the native label announced for this input. The shared input is
    /// used in several surfaces, so callers own the task-specific name while
    /// value, placeholder, focus, and selection stay centralized here.
    pub fn accessibility_label(mut self, label: impl Into<String>) -> Self {
        self.accessibility_label = Some(label.into());
        self
    }

    /// Keep the input's value out of the accessibility tree while preserving
    /// normal visual editing and input behavior.
    pub fn accessibility_secure(mut self, secure: bool) -> Self {
        self.accessibility_secure = secure;
        self
    }

    /// Identify a search editor on its actual editable accessibility node.
    pub fn accessibility_search(mut self, search: bool) -> Self {
        self.accessibility_search = search;
        self
    }
}

/// Action listener that runs `handler` on the input's state entity.
fn forward<A: Action>(
    state: &Entity<InputState>,
    handler: fn(&mut InputState, &A, &mut Window, &mut Context<InputState>),
) -> impl Fn(&A, &mut Window, &mut App) + 'static {
    let state = state.clone();
    move |action, window, cx| state.update(cx, |state, cx| handler(state, action, window, cx))
}

fn accessibility_value(text: &str, secure: bool) -> String {
    if secure {
        "•".repeat(text.chars().count())
    } else {
        text.to_string()
    }
}

impl RenderOnce for Input {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        if crate::bench::ui_fixture::take_feedback_bidi_input_request() {
            bidi_probe::load_bidi_input_text(&self.state, window, cx);
        }
        let state = self.state.read(cx);
        let text = state.value().to_string();
        let cursor = state.cursor();
        let cursor_affinity = state.cursor_affinity();
        let selection = state.selection();
        let marked_range = state.marked_range();
        let placeholder = state.placeholder().to_string();
        let is_disabled = self.disabled || state.is_disabled();
        let focus_handle = state.focus_handle().clone();
        let is_focused = focus_handle.is_focused(window);
        let spell_check_ranges = state.spell_check_ranges().to_vec();
        let accessibility_label = self.accessibility_label;

        let state_entity = self.state.clone();

        let text_style = window.text_style();
        let font_size = text_style.font_size;
        let line_height = font_size.to_pixels(window.rem_size()) * 1.4;

        let text_color = if is_disabled {
            colors::text_muted()
        } else {
            colors::text_primary()
        };
        let placeholder_color = colors::text_muted();
        let selection_color = colors::accent_transparent(0.32);
        let cursor_color = colors::accent();

        let display_text = if text.is_empty() {
            placeholder.clone()
        } else {
            text.clone()
        };
        let is_placeholder = text.is_empty();

        let byte_to_character = |offset: usize| text[..offset.min(text.len())].chars().count();
        let selection_positions = selection.as_ref().map(|selection| {
            let (anchor, focus) = if cursor == selection.start {
                (selection.end, selection.start)
            } else {
                (selection.start, selection.end)
            };
            (byte_to_character(anchor), byte_to_character(focus))
        });
        let cursor_position = byte_to_character(cursor);
        let a11y_text = accessibility_value(&text, self.accessibility_secure);
        let accessibility_placeholder = if self.accessibility_secure {
            "Sensitive value".to_string()
        } else {
            placeholder.clone()
        };
        let accessibility_role = if self.accessibility_secure {
            Role::PasswordInput
        } else if self.accessibility_search {
            Role::SearchInput
        } else {
            Role::TextInput
        };

        let container = div()
            .id("echo-input")
            .role(accessibility_role)
            .aria_value(a11y_text.clone())
            .aria_placeholder(accessibility_placeholder)
            .a11y_synthetic_children(move |builder| {
                crate::components::ui::accessibility::apply_description(builder);
                if let Some(label) = &accessibility_label {
                    builder.parent_node().set_label(label.clone());
                }
                if builder.parent_node().label().is_none() {
                    builder.parent_node().set_label("Text input".to_string());
                }
                let mut run = accesskit::Node::new(Role::TextRun);
                run.set_value(a11y_text.clone());
                run.set_character_lengths(
                    a11y_text
                        .chars()
                        .map(|character| character.len_utf8() as u8)
                        .collect::<Vec<_>>(),
                );
                let run_id = builder.synthetic_node_id(0);
                builder.push_child(run_id, run);

                let (anchor, focus) =
                    selection_positions.unwrap_or((cursor_position, cursor_position));
                builder
                    .parent_node()
                    .set_text_selection(accesskit::TextSelection {
                        anchor: accesskit::TextPosition {
                            node: run_id,
                            character_index: anchor,
                        },
                        focus: accesskit::TextPosition {
                            node: run_id,
                            character_index: focus,
                        },
                    });
            })
            .key_context(CONTEXT);

        let mut container = if is_disabled {
            container.track_focus(&focus_handle).child(
                crate::components::ui::accessibility::focus_marker(focus_handle.clone()),
            )
        } else {
            crate::components::ui::accessibility::accessible_focus(container, focus_handle.clone())
        }
        .w_full()
        .flex()
        .items_center()
        .h(line_height + px(8.0))
        .px(px(Spacing::SM))
        .cursor_text()
        .overflow_hidden();

        if self.appearance {
            container = container.bg(colors::bg_surface()).rounded(Radii::sm());

            if self.bordered {
                container = container.border_1().border_color(colors::border());
            }

            if self.focus_bordered && is_focused {
                container =
                    container.border_color(colors::settings_focus_ring(colors::bg_surface()));
            }
        }

        // UI Automation's ValuePattern drives `SetValue`, while assistive
        // technology that edits an existing selection uses
        // `ReplaceSelectedText`. Keep both routes in InputState so they share
        // the same single-line normalization, undo, cursor, and Change-event
        // behavior as keyboard, paste, and IME input.
        if !is_disabled {
            container = container
                .on_a11y_action(accesskit::Action::SetValue, {
                    let state = state_entity.clone();
                    move |data, window, cx| {
                        let Some(accesskit::ActionData::Value(value)) = data else {
                            return;
                        };
                        let value = value.to_string();
                        state.update(cx, |state, cx| state.set_value(value, window, cx));
                    }
                })
                .on_a11y_action(accesskit::Action::ReplaceSelectedText, {
                    let state = state_entity.clone();
                    move |data, window, cx| {
                        let Some(accesskit::ActionData::Value(value)) = data else {
                            return;
                        };
                        let value = value.to_string();
                        state.update(cx, |state, cx| state.insert(&value, window, cx));
                    }
                });
        }

        container = container
            .on_action(forward(&state_entity, InputState::handle_backspace))
            .on_action(forward(&state_entity, InputState::handle_delete))
            .on_action(forward(&state_entity, InputState::handle_delete_word))
            .on_action(forward(
                &state_entity,
                InputState::handle_delete_word_forward,
            ))
            .on_action(forward(&state_entity, InputState::handle_enter))
            .on_action(forward(&state_entity, InputState::handle_escape))
            .on_action(forward(&state_entity, InputState::handle_move_left))
            .on_action(forward(&state_entity, InputState::handle_move_right))
            .on_action(forward(&state_entity, InputState::handle_move_to_start))
            .on_action(forward(&state_entity, InputState::handle_move_to_end))
            .on_action(forward(&state_entity, InputState::handle_move_word_left))
            .on_action(forward(&state_entity, InputState::handle_move_word_right))
            .on_action(forward(&state_entity, InputState::handle_select_left))
            .on_action(forward(&state_entity, InputState::handle_select_right))
            .on_action(forward(&state_entity, InputState::handle_select_to_start))
            .on_action(forward(&state_entity, InputState::handle_select_to_end))
            .on_action(forward(&state_entity, InputState::handle_select_word_left))
            .on_action(forward(&state_entity, InputState::handle_select_word_right))
            .on_action(forward(&state_entity, InputState::handle_select_all))
            .on_action(forward(&state_entity, InputState::handle_copy))
            .on_action(forward(&state_entity, InputState::handle_cut))
            .on_action(forward(&state_entity, InputState::handle_paste))
            .on_action(forward(&state_entity, InputState::handle_undo))
            .on_action(forward(&state_entity, InputState::handle_redo));

        if !is_disabled {
            container = container
                .on_mouse_down(MouseButton::Left, {
                    let focus_handle = focus_handle.clone();
                    let state = state_entity.clone();
                    move |event: &MouseDownEvent, window, cx| {
                        window.focus(&focus_handle, cx);
                        state.update(cx, |state, cx| state.handle_pointer_down(event, cx));
                    }
                })
                .on_mouse_move({
                    let state = state_entity.clone();
                    move |event: &MouseMoveEvent, _window, cx| {
                        state.update(cx, |state, cx| state.handle_pointer_move(event, cx));
                    }
                })
                .on_mouse_up(MouseButton::Left, {
                    let state = state_entity.clone();
                    move |event: &MouseUpEvent, _window, cx| {
                        state.update(cx, |state, cx| state.handle_pointer_up(event, cx));
                    }
                })
                .on_mouse_up_out(MouseButton::Left, {
                    let state = state_entity.clone();
                    move |event: &MouseUpEvent, _window, cx| {
                        state.update(cx, |state, cx| state.handle_pointer_up(event, cx));
                    }
                });
        }

        let text_canvas = {
            let display_text = display_text.clone();
            let layout_text = display_text.clone();
            let cursor = if is_placeholder { 0 } else { cursor };
            let cursor_affinity = if is_placeholder {
                CaretAffinity::Leading
            } else {
                cursor_affinity
            };
            let selection: Option<Range<usize>> = if is_placeholder { None } else { selection };
            let marked_range = if is_placeholder { None } else { marked_range };
            let text_color = if is_placeholder {
                placeholder_color
            } else {
                text_color
            };
            let spell_ranges = if is_placeholder {
                vec![]
            } else {
                spell_check_ranges
            };
            let focus_handle_for_ime = focus_handle.clone();

            canvas(
                move |bounds, window, cx| {
                    let text_style = window.text_style();
                    let font = text_style.font();
                    let font_size = text_style.font_size;
                    let rem = window.rem_size();

                    let runs = vec![TextRun {
                        len: display_text.len(),
                        font: font.clone(),
                        color: text_color,
                        background_color: None,
                        underline: None,
                        strikethrough: None,
                    }];

                    let font_size_px = font_size.to_pixels(rem);
                    let line = window.text_system().shape_line(
                        display_text.clone().into(),
                        font_size_px,
                        &runs,
                        None,
                    );
                    let line_height_px = font_size_px * 1.4;
                    let text_top = bounds.origin.y + (bounds.size.height - line_height_px) / 2.0;

                    let exact_interaction_geometry =
                        !line.has_bidi_runs || line.bidi_geometry_complete;
                    let cursor_x = exact_interaction_geometry
                        .then(|| {
                            line.x_for_index_with_affinity_checked(
                                cursor.min(line.len()),
                                cursor_affinity,
                            )
                        })
                        .flatten();

                    let scroll_x = state_entity.update(cx, |state, _| {
                        let scroll_x = state.follow_caret(
                            cursor_x.filter(|_| is_focused),
                            line.width(),
                            bounds.size.width,
                            CARET_WIDTH,
                        );
                        state.update_pointer_layout(
                            Bounds {
                                origin: point(bounds.origin.x - scroll_x, bounds.origin.y),
                                size: size(bounds.size.width + scroll_x, bounds.size.height),
                            },
                            line.clone(),
                            &layout_text,
                            &runs[0],
                            font_size_px,
                            line_height_px,
                        );
                        scroll_x
                    });
                    let text_left = bounds.origin.x - scroll_x;
                    let text_origin = point(text_left, text_top);

                    let selection_bounds: Vec<Bounds<Pixels>> = selection
                        .as_ref()
                        .filter(|_| exact_interaction_geometry)
                        .map(|selection| {
                            line.visual_spans_for_range(selection.clone())
                                .into_iter()
                                .map(|span| Bounds {
                                    origin: point(text_left + span.start, text_top),
                                    size: size(span.end - span.start, line_height_px),
                                })
                                .collect()
                        })
                        .unwrap_or_default();

                    let spell_underline_bounds: Vec<Bounds<gpui::Pixels>> = spell_ranges
                        .iter()
                        .filter(|_| exact_interaction_geometry)
                        .flat_map(|range| {
                            line.visual_spans_for_range(range.clone())
                                .into_iter()
                                .map(|span| {
                                    let underline_y = text_top + line_height_px - px(2.0);
                                    Bounds {
                                        origin: point(text_left + span.start, underline_y),
                                        size: size(span.end - span.start, px(1.5)),
                                    }
                                })
                        })
                        .collect();

                    let marked_underline_bounds: Vec<Bounds<gpui::Pixels>> = marked_range
                        .as_ref()
                        .filter(|range| {
                            range.start < range.end
                                && range.end <= display_text.len()
                                && display_text.is_char_boundary(range.start)
                                && display_text.is_char_boundary(range.end)
                        })
                        .filter(|_| exact_interaction_geometry)
                        .map(|range| {
                            line.visual_spans_for_range(range.clone())
                                .into_iter()
                                .map(|span| {
                                    let underline_y = text_top + line_height_px - px(2.0);
                                    Bounds {
                                        origin: point(text_left + span.start, underline_y),
                                        size: size(span.end - span.start, px(1.5)),
                                    }
                                })
                                .collect()
                        })
                        .unwrap_or_default();

                    if crate::bench::ui_fixture::is_feedback_bidi_input_text(&display_text) {
                        bidi_probe::publish_bidi_input_probe(
                            &line,
                            &display_text,
                            bounds,
                            cursor,
                            cursor_affinity,
                            selection.as_ref(),
                            &selection_bounds,
                        );
                    }

                    let cursor_bounds = if is_focused && exact_interaction_geometry {
                        cursor_x.map(|cursor_x| Bounds {
                            origin: point(text_left + cursor_x, text_top + px(2.0)),
                            size: size(CARET_WIDTH, line_height_px - px(4.0)),
                        })
                    } else {
                        None
                    };

                    PrepaintState {
                        bounds,
                        text_origin,
                        line_height_px,
                        line,
                        selection_bounds,
                        marked_underline_bounds,
                        spell_underline_bounds,
                        cursor_bounds,
                        focus_handle: focus_handle_for_ime.clone(),
                        state_entity: state_entity.clone(),
                    }
                },
                move |bounds, prepaint, window, cx| {
                    window.with_content_mask(Some(ContentMask { bounds }), |window| {
                        for selection_bounds in &prepaint.selection_bounds {
                            window.paint_quad(fill(*selection_bounds, selection_color));
                        }

                        let _ = prepaint.line.paint(
                            prepaint.text_origin,
                            prepaint.line_height_px,
                            TextAlign::Left,
                            None,
                            window,
                            cx,
                        );

                        let spell_underline_color = Hsla {
                            h: 0.0,
                            s: 0.8,
                            l: 0.5,
                            a: 1.0,
                        };
                        for underline_bounds in &prepaint.marked_underline_bounds {
                            window.paint_quad(fill(*underline_bounds, colors::accent()));
                        }
                        for underline_bounds in &prepaint.spell_underline_bounds {
                            window.paint_quad(fill(*underline_bounds, spell_underline_color));
                        }

                        if let Some(cursor_bounds) = prepaint.cursor_bounds {
                            window.paint_quad(fill(cursor_bounds, cursor_color));
                        }
                    });

                    window.handle_input(
                        &prepaint.focus_handle,
                        ElementInputHandler::new(prepaint.bounds, prepaint.state_entity.clone()),
                        cx,
                    );
                },
            )
            .w_full()
            .h_full()
        };

        container.child(text_canvas)
    }
}

#[cfg(test)]
mod tests {
    use super::accessibility_value;
    use core::prelude::v1::test;

    #[test]
    fn secure_accessibility_value_masks_the_complete_secret() {
        let secret = "Authorization: Bearer canary-api-key-123";
        let value = accessibility_value(secret, true);

        assert!(!value.contains("canary-api-key-123"));
        assert_eq!(value.chars().count(), secret.chars().count());
        assert!(value.chars().all(|character| character == '•'));
    }

    #[test]
    fn ordinary_accessibility_value_remains_readable() {
        assert_eq!(accessibility_value("Request URL", false), "Request URL");
    }
}
