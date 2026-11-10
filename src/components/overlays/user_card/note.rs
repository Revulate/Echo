//! Local per-user note: lazily created input, debounced auto-save, and the
//! note row rendered under the card's action buttons.

use super::UserCard;
use crate::backend;
use crate::components::ui::ButtonExt;
use crate::components::ui::input::{Input, InputEvent, InputState, apply_standard_input_chrome};
use crate::theme::colors;
use crate::theme::tokens::{Radii, RadiiPixels};
use gpui::prelude::FluentBuilder;
use gpui::*;
use std::time::Duration;

const USER_CARD_NOTE_MAX_LEN: usize = 180;

/// Idle time after the last keystroke before the local note auto-saves.
const NOTE_AUTOSAVE_DEBOUNCE_MS: u64 = 800;

/// Local-note editor. The input is created on first render and rebound to the
/// current user's stored note whenever the card switches user.
#[derive(Default)]
pub(super) struct NoteEditor {
    input: Option<Entity<InputState>>,
    bound_user: Option<String>,
    /// Auto-save runs constantly, so success is silent; only a failure or an
    /// over-length note surfaces a status line.
    pub(super) error: Option<String>,
    /// Unsaved note edits pending the auto-save debounce.
    dirty: bool,
    last_edit: Option<std::time::Instant>,
    autosave_waker_running: bool,
    /// Focus target used to finish note editing without closing the card.
    dismiss_focus: Option<FocusHandle>,
}

impl NoteEditor {
    /// Drop any pending auto-save; the caller saves right away instead.
    fn clear_pending_edit(&mut self) {
        self.dirty = false;
        self.last_edit = None;
    }

    /// Forget the bound user so the next render loads the new user's note.
    pub(super) fn unbind(&mut self) {
        self.bound_user = None;
        self.error = None;
        self.clear_pending_edit();
    }
}

impl UserCard {
    pub(super) fn ensure_note_input(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
        username: &str,
        current_note: &str,
    ) {
        if self.note.dismiss_focus.is_none() {
            self.note.dismiss_focus = Some(cx.focus_handle());
        }

        if self.note.input.is_none() {
            let input =
                cx.new(|cx| InputState::new(window, cx).with_placeholder("Add local note…"));
            let subscription =
                cx.subscribe(&input, |this, input, event: &InputEvent, cx| match event {
                    InputEvent::Change => {
                        let value = input.read(cx).value();
                        this.note.error =
                            (value.chars().count() > USER_CARD_NOTE_MAX_LEN).then(|| {
                                format!(
                                    "Local notes are limited to {} characters",
                                    USER_CARD_NOTE_MAX_LEN
                                )
                            });
                        this.note.dirty = true;
                        this.note.last_edit = Some(std::time::Instant::now());
                        this.start_note_autosave_waker(cx);
                        cx.notify();
                    }
                    InputEvent::PressEnter => {
                        if let Some(username) = this.state.username.clone() {
                            this.note.clear_pending_edit();
                            this.save_note_from_input(&username, cx);
                        }
                        if let Some(focus_handle) = this.note.dismiss_focus.clone() {
                            let entity_id = cx.entity_id();
                            let _ = cx.with_window(entity_id, |window, app| {
                                window.focus(&focus_handle, app);
                            });
                        }
                    }
                    InputEvent::Blur if this.note.dirty => {
                        this.note.clear_pending_edit();
                        if let Some(username) = this.state.username.clone() {
                            this.save_note_from_input(&username, cx);
                        }
                    }
                    _ => {}
                });

            self.note.input = Some(input);
            self.subscriptions.push(subscription);
        }

        if self.note.bound_user.as_deref() != Some(username) {
            if let Some(input) = self.note.input.clone() {
                input.update(cx, |state, cx| {
                    state.set_value(current_note.to_string(), window, cx);
                });
            }
            self.note.bound_user = Some(username.to_string());
            self.note.error = None;
        }
    }

    /// Persist the note input. Auto-save runs constantly, so success is
    /// silent; only failures surface a status line.
    fn save_note_from_input(&mut self, username: &str, cx: &mut Context<Self>) {
        let Some(input) = self.note.input.clone() else {
            return;
        };
        let note = truncate_note_input(input.read(cx).value());
        let note = note.trim().to_string();

        let result = if note.is_empty() {
            backend::user_notes::delete_user_note(username).map(|_| ())
        } else {
            backend::user_notes::set_user_note(username, note).map(|_| ())
        };

        self.note.error = result.err();
        cx.notify();
    }

    /// Debounced auto-save loop for the persistent note box.
    fn start_note_autosave_waker(&mut self, cx: &mut Context<Self>) {
        if self.note.autosave_waker_running {
            return;
        }
        self.note.autosave_waker_running = true;
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(NOTE_AUTOSAVE_DEBOUNCE_MS / 2))
                    .await;

                let keep_running = this
                    .update(cx, |card, cx| {
                        if !card.note.dirty {
                            card.note.autosave_waker_running = false;
                            return false;
                        }
                        let idle_long_enough = card
                            .note
                            .last_edit
                            .map(|at| at.elapsed().as_millis() as u64 >= NOTE_AUTOSAVE_DEBOUNCE_MS)
                            .unwrap_or(true);
                        if idle_long_enough {
                            card.note.dirty = false;
                            card.note.autosave_waker_running = false;
                            if let Some(username) = card.state.username.clone() {
                                card.save_note_from_input(&username, cx);
                            }
                            return false;
                        }
                        true
                    })
                    .unwrap_or(false);

                if !keep_running {
                    break;
                }
            }
        })
        .detach();
    }

    pub(super) fn render_user_note_section(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let is_focused = self
            .note
            .input
            .as_ref()
            .is_some_and(|input| input.read(cx).focus_handle().is_focused(window));
        let editor = if let Some(input) = self.note.input.as_ref() {
            let focus_handle = input.read(cx).focus_handle().clone();

            apply_standard_input_chrome(
                div()
                    .id("user-card-note-shell")
                    .flex_1()
                    .min_w_0()
                    .min_h(px(26.0))
                    .px_2()
                    .cursor_text()
                    .flex()
                    .items_center()
                    .on_mouse_down(MouseButton::Left, move |_event, window, cx| {
                        window.focus(&focus_handle, cx);
                        window.prevent_default();
                    }),
                is_focused,
            )
            .child(
                div()
                    .flex_1()
                    .text_xs()
                    .text_color(colors::text_primary())
                    .child(
                        Input::new(input)
                            .appearance(false)
                            .bordered(false)
                            .focus_bordered(false),
                    ),
            )
            .into_any_element()
        } else {
            div()
                .flex_1()
                .text_xs()
                .text_color(colors::text_muted())
                .child("Loading note editor…")
                .into_any_element()
        };

        let dismiss_focus = self.note.dismiss_focus.clone();

        div()
            .flex()
            .items_center()
            .gap_1()
            .when_some(dismiss_focus.as_ref(), |row, focus_handle| {
                row.track_focus(focus_handle)
            })
            .child(editor)
            .when(is_focused, |row| {
                row.child(
                    div()
                        .id("user-card-note-done")
                        .px_2()
                        .min_h(px(26.0))
                        .rounded(Radii::sm())
                        .cursor_pointer()
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_xs()
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(colors::accent_text())
                        .hover(|style| style.bg(colors::accent_transparent(0.12)))
                        .on_activate(cx.listener(move |this, _event, window, cx| {
                            this.note.clear_pending_edit();
                            if let Some(username) = this.state.username.clone() {
                                this.save_note_from_input(&username, cx);
                            }
                            if let Some(focus_handle) = this.note.dismiss_focus.as_ref() {
                                window.focus(focus_handle, cx);
                            } else {
                                window.blur(cx);
                            }
                            cx.notify();
                        }))
                        .child("Done"),
                )
            })
    }
}

fn truncate_note_input(note: &str) -> String {
    note.trim().chars().take(USER_CARD_NOTE_MAX_LEN).collect()
}
