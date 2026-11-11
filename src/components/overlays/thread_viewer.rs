//! Thread viewer overlay
//!
//! Floating window for viewing reply chains:
//! - Draggable header
//! - Resizable (280-600px width, 150-700px height)
//! - Root message + all replies
//! - Reply input within thread
//! - Close on Escape

use super::traits::{Openable, OverlayPointerIsolation};
use super::{
    Autocomplete, AutocompleteHelper, AutocompleteState, AutocompleteSuggestion, AutocompleteType,
    InlineTabCompletionSession,
};
use super::{ContextMenuState, MessageMenuTarget};
use crate::backend::{self, ChatMessage};
use crate::components::ui::ButtonExt;
use crate::components::ui::input::{
    Input, InputEvent, InputState, THREAD_REPLY_INPUT_CONTEXT, TabComplete,
};
use crate::state::{SplitId, VersionRegistry};
use crate::theme::colors;
use crate::theme::tokens::{Radii, RadiiPixels};
use crate::utils::format_message_timestamp;
use gpui::prelude::FluentBuilder;
use gpui::*;
use std::collections::{HashMap, HashSet};

const THREAD_VIEWER_REFRESH_INTERVAL_MS: u64 = 400;
const THREAD_VIEWER_REFRESH_TICKS: usize = 20;
const THREAD_VIEWER_WINDOW_MARGIN: f32 = 8.0;
const THREAD_COMPLETION_MAX_ROWS: usize = 6;
const THREAD_COMPLETION_ROW_HEIGHT: f32 = 36.0;
const THREAD_COMPLETION_IMAGE_SIZE: f32 = 26.0;
const THREAD_COMPOSER_HEIGHT: f32 = 44.0;
const THREAD_SEND_RAIL_WIDTH: f32 = 64.0;

/// Thread viewer state
#[derive(Clone, Debug, Default)]
pub struct ThreadViewerState {
    pub is_open: bool,
    /// ID of the root message (the one being viewed in thread context)
    pub root_message_id: Option<String>,
    /// Channel where the thread exists
    pub channel: Option<String>,
    /// Position on screen
    pub x: f32,
    pub y: f32,
    /// Current size
    pub width: f32,
    pub height: f32,
}

impl Openable for ThreadViewerState {
    fn set_open(&mut self, open: bool) {
        self.is_open = open;
    }
}

impl ThreadViewerState {
    /// Close the thread viewer and clear state
    pub fn close(&mut self) {
        Openable::close(self);
        self.root_message_id = None;
        self.channel = None;
    }
}

/// Drag mode for window movement
#[derive(Clone, Debug, PartialEq)]
enum DragMode {
    Move,
}

/// Thread viewer component
pub struct ThreadViewer {
    pub state: ThreadViewerState,
    /// Cached thread messages (root + replies)
    thread_messages: Vec<ChatMessage>,
    /// Native Echo input used for thread replies.
    reply_input: Option<Entity<InputState>>,
    /// Popup-local completion model. A thread reply is not a normal split, so
    /// routing it through EchoApp's split-targeted autocomplete would be stale.
    reply_autocomplete: Autocomplete,
    completion_target: SplitId,
    tab_completion: Option<InlineTabCompletionSession>,
    completion_edit_in_progress: bool,
    pending_clear_reply_input: bool,
    focus_reply_input_on_open: bool,
    _subscriptions: Vec<Subscription>,
    /// Drag state
    drag_mode: Option<DragMode>,
    drag_start: Point<Pixels>,
    initial_pos: (f32, f32),
    refresh_waker_running: bool,
    /// Inputs the cached snapshot was built from. The deleted-message mode is
    /// part of the key because it filters/rewrites rows without touching the
    /// chat version.
    refresh_gate: Option<ThreadRefreshGate>,
}

/// Snapshot inputs for `ThreadViewer::refresh`.
type ThreadRefreshGate = (u64, crate::backend::settings::DeletedMessageMode);

impl ThreadViewer {
    pub fn new(state: ThreadViewerState) -> Self {
        let refresh_gate = Some(Self::refresh_gate_key());
        let thread_messages = Self::fetch_thread_messages(&state);
        Self {
            state,
            thread_messages,
            reply_input: None,
            reply_autocomplete: Autocomplete::new(AutocompleteState::default()),
            completion_target: SplitId::new(),
            tab_completion: None,
            completion_edit_in_progress: false,
            pending_clear_reply_input: false,
            focus_reply_input_on_open: false,
            _subscriptions: Vec::new(),
            drag_mode: None,
            drag_start: Point::default(),
            initial_pos: (0.0, 0.0),
            refresh_waker_running: false,
            refresh_gate,
        }
    }

    /// Check if the viewer is open
    pub fn is_open(&self) -> bool {
        self.state.is_open
    }

    fn fetch_thread_messages(state: &ThreadViewerState) -> Vec<ChatMessage> {
        let Some(ref msg_id) = state.root_message_id else {
            return Vec::new();
        };
        let Some(ref channel) = state.channel else {
            return Vec::new();
        };

        let all_messages =
            backend::get_messages(channel, crate::config::MAX_CACHED_MESSAGES as u32);
        let mut thread_msgs = Self::collect_retained_thread(&all_messages, msg_id);
        if matches!(
            crate::backend::deleted_message_display::action_for_deleted_message_mode(
                crate::backend::settings::get_deleted_message_mode(),
            ),
            crate::backend::deleted_message_display::DeletedMessageDisplayAction::RemoveOriginal
        ) {
            thread_msgs.retain(|msg| !msg.flags.is_deleted);
        }
        thread_msgs
    }

    /// Resolve the selected message to its oldest retained ancestor, then walk
    /// the complete retained descendant tree. This keeps nested replies and
    /// siblings together instead of returning only one direct-reply level.
    fn collect_retained_thread(
        all_messages: &[ChatMessage],
        selected_message_id: &str,
    ) -> Vec<ChatMessage> {
        let by_id: HashMap<&str, usize> = all_messages
            .iter()
            .enumerate()
            .map(|(index, message)| (message.id.as_ref(), index))
            .collect();
        let Some(&selected_index) = by_id.get(selected_message_id) else {
            return Vec::new();
        };

        let mut root_index = selected_index;
        let mut ancestor_indices = HashSet::new();
        while ancestor_indices.insert(root_index) {
            let Some(parent_id) = all_messages[root_index]
                .reply
                .as_ref()
                .map(|reply| reply.parent_msg_id.as_ref())
            else {
                break;
            };
            let Some(&parent_index) = by_id.get(parent_id) else {
                break;
            };
            root_index = parent_index;
        }

        let mut children: HashMap<&str, Vec<usize>> = HashMap::new();
        for (index, message) in all_messages.iter().enumerate() {
            if let Some(reply) = message.reply.as_ref() {
                children
                    .entry(reply.parent_msg_id.as_ref())
                    .or_default()
                    .push(index);
            }
        }

        let mut stack = vec![root_index];
        let mut included_indices = HashSet::new();
        let mut thread_messages = Vec::new();
        while let Some(index) = stack.pop() {
            if !included_indices.insert(index) {
                continue;
            }

            let message = &all_messages[index];
            thread_messages.push(message.clone());
            if let Some(child_indices) = children.get(message.id.as_ref()) {
                stack.extend(child_indices.iter().rev().copied());
            }
        }

        thread_messages.sort_by_key(|message| message.timestamp);
        thread_messages.dedup_by(|left, right| left.id == right.id);
        thread_messages
    }

    fn refresh_gate_key() -> ThreadRefreshGate {
        (
            VersionRegistry::chat_version(),
            crate::backend::settings::get_deleted_message_mode(),
        )
    }

    /// Refresh thread messages from backend
    pub fn refresh(&mut self) {
        self.refresh_gate = Some(Self::refresh_gate_key());
        self.thread_messages = Self::fetch_thread_messages(&self.state);
    }

    fn thread_messages_changed(before: &[ChatMessage], after: &[ChatMessage]) -> bool {
        before.len() != after.len()
            || before.iter().zip(after).any(|(left, right)| {
                left.id != right.id
                    || left.timestamp != right.timestamp
                    || left.flags.is_deleted != right.flags.is_deleted
                    || left.content != right.content
            })
    }

    pub fn open_thread(&mut self, message_id: String, channel: String, x: f32, y: f32) {
        self.state.is_open = true;
        self.state.root_message_id = Some(message_id);
        self.state.channel = Some(channel);
        self.state.x = x;
        self.state.y = y;
        if self.state.width <= 0.0 {
            self.state.width = 420.0;
        }
        if self.state.height <= 0.0 {
            self.state.height = 320.0;
        }

        self.pending_clear_reply_input = true;
        self.focus_reply_input_on_open = true;
        self.close_reply_autocomplete();
        self.tab_completion = None;
        self.drag_mode = None;
        self.refresh();
    }

    fn ensure_reply_input(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.reply_input.is_some() {
            return;
        }

        let reply_input =
            cx.new(|cx| InputState::new(window, cx).with_placeholder("Reply in thread…"));
        let subscription =
            cx.subscribe(
                &reply_input,
                |this, input, event: &InputEvent, cx| match event {
                    InputEvent::Change => {
                        if this.completion_edit_in_progress {
                            this.completion_edit_in_progress = false;
                        } else {
                            this.tab_completion = None;
                            let input = input.read(cx);
                            this.update_reply_autocomplete(input.value(), input.cursor());
                        }
                        cx.notify();
                    }
                    InputEvent::PressEnter => {
                        if !this.reply_autocomplete.state.is_open {
                            this.send_reply(cx);
                        }
                    }
                    InputEvent::Blur => {
                        this.close_reply_autocomplete();
                        this.tab_completion = None;
                        cx.notify();
                    }
                    InputEvent::Focus => {}
                },
            );

        self.reply_input = Some(reply_input);
        self._subscriptions.push(subscription);
    }

    fn update_reply_autocomplete(&mut self, text: &str, cursor: usize) {
        let context = AutocompleteHelper::detect_trigger(text, cursor).filter(|context| {
            matches!(
                context.ac_type,
                AutocompleteType::Emote | AutocompleteType::Mention
            )
        });
        let Some(context) = context else {
            self.close_reply_autocomplete();
            return;
        };
        let Some(channel) = self.state.channel.clone() else {
            self.close_reply_autocomplete();
            return;
        };

        self.reply_autocomplete
            .update_state(AutocompleteState::open(
                0.0,
                0.0,
                channel,
                self.completion_target,
                context,
            ));
        self.reply_autocomplete
            .suggestions
            .truncate(THREAD_COMPLETION_MAX_ROWS);
        if self.reply_autocomplete.state.selected_index >= self.reply_autocomplete.suggestions.len()
        {
            self.reply_autocomplete.state.selected_index = 0;
        }
    }

    fn refresh_reply_autocomplete_from_input(&mut self, cx: &App) {
        let Some(reply_input) = self.reply_input.clone() else {
            self.close_reply_autocomplete();
            return;
        };
        let (text, cursor) = {
            let input = reply_input.read(cx);
            (input.value().to_string(), input.cursor())
        };
        self.update_reply_autocomplete(&text, cursor);
    }

    fn close_reply_autocomplete(&mut self) {
        self.reply_autocomplete.state.close();
    }

    fn move_reply_autocomplete_selection(&mut self, direction: isize) {
        let len = self.reply_autocomplete.suggestions.len();
        if len == 0 {
            return;
        }

        let selected = self.reply_autocomplete.state.selected_index.min(len - 1);
        self.reply_autocomplete.state.selected_index = if direction < 0 {
            selected.checked_sub(1).unwrap_or(len - 1)
        } else {
            (selected + 1) % len
        };
    }

    fn accept_reply_autocomplete(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let revision = self.reply_autocomplete.suggestion_revision();
        let Some(selection) = self.reply_autocomplete.selection_at(revision, index) else {
            return false;
        };
        let Some(reply_input) = self.reply_input.clone() else {
            return false;
        };

        self.completion_edit_in_progress = true;
        let applied = reply_input.update(cx, |input, cx| {
            input.replace_exact_range_with_token(
                selection.context.replacement_range.clone(),
                &selection.context.expected_fragment,
                &selection.text,
                window,
                cx,
            )
        });
        if !applied {
            self.completion_edit_in_progress = false;
        } else if selection.is_emote {
            self.record_reply_emote_usage(
                &selection.channel,
                &selection.text,
                selection.emote_image_url.as_deref(),
            );
        }

        self.close_reply_autocomplete();
        self.tab_completion = None;
        let focus_handle = reply_input.read(cx).focus_handle().clone();
        window.focus(&focus_handle, cx);
        cx.notify();
        applied
    }

    fn cycle_inline_tab_completion(
        &mut self,
        reverse: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(reply_input) = self.reply_input.clone() else {
            return false;
        };
        let (text, cursor) = {
            let input = reply_input.read(cx);
            (input.value().to_string(), input.cursor())
        };

        let session_matches = self
            .tab_completion
            .as_ref()
            .is_some_and(|session| session.matches(&text, cursor));
        if !session_matches {
            self.tab_completion = None;
            let Some(context) = AutocompleteHelper::detect_tab_context(&text, cursor) else {
                return false;
            };
            let Some(channel) = self.state.channel.as_deref() else {
                return false;
            };

            let candidates = self.reply_autocomplete.tab_candidates(channel, &context);
            self.tab_completion = InlineTabCompletionSession::new(context, candidates);
        }

        let Some(edit) = self
            .tab_completion
            .as_mut()
            .map(|session| session.next_edit(reverse))
        else {
            return false;
        };

        self.completion_edit_in_progress = true;
        let applied = reply_input.update(cx, |input, cx| {
            input.replace_exact_range_with_token(
                edit.replacement_range.clone(),
                &edit.expected_fragment,
                &edit.text,
                window,
                cx,
            )
        });
        if !applied {
            self.completion_edit_in_progress = false;
            self.tab_completion = None;
            return false;
        }

        let cursor = reply_input.read(cx).cursor();
        if let Some(session) = self.tab_completion.as_mut() {
            session.commit_applied(&edit, cursor);
        }
        if edit.is_emote
            && let Some(channel) = self.state.channel.as_deref()
        {
            self.record_reply_emote_usage(channel, &edit.text, None);
        }
        cx.notify();
        true
    }

    fn handle_reply_tab_complete_action(
        &mut self,
        reverse: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.reply_autocomplete.state.is_open {
            if self.reply_autocomplete.suggestions.is_empty() {
                self.close_reply_autocomplete();
                cx.notify();
                return true;
            }
            return self.accept_reply_autocomplete(
                self.reply_autocomplete.state.selected_index,
                window,
                cx,
            );
        }

        self.cycle_inline_tab_completion(reverse, window, cx)
    }

    fn record_reply_emote_usage(&self, channel: &str, emote_name: &str, image_url: Option<&str>) {
        if let Some(emote) = self
            .reply_autocomplete
            .accepted_emote(channel, emote_name, image_url)
        {
            super::record_emote_usage(&emote);
        }
    }

    pub(crate) fn prepare_completion_smoke(
        &mut self,
        text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<crate::bench::ui_fixture::CompletionUiSnapshot> {
        if !crate::bench::ui_fixture::is_enabled() || !self.state.is_open {
            return None;
        }
        self.ensure_reply_input(window, cx);
        let reply_input = self.reply_input.clone()?;
        if !reply_input.update(cx, |input, cx| {
            input.set_value_for_completion_smoke(text, cx)
        }) {
            return None;
        }
        self.tab_completion = None;
        self.completion_edit_in_progress = false;
        self.update_reply_autocomplete(text, text.len());
        let focus_handle = reply_input.read(cx).focus_handle().clone();
        window.focus(&focus_handle, cx);
        cx.notify();
        self.completion_smoke_snapshot(window, cx)
    }

    pub(crate) fn completion_smoke_snapshot(
        &self,
        window: &Window,
        cx: &App,
    ) -> Option<crate::bench::ui_fixture::CompletionUiSnapshot> {
        if !crate::bench::ui_fixture::is_enabled() || !self.state.is_open {
            return None;
        }
        let input = self.reply_input.as_ref()?.read(cx);
        let selected_text = self
            .reply_autocomplete
            .selection_at(
                self.reply_autocomplete.suggestion_revision(),
                self.reply_autocomplete.state.selected_index,
            )
            .map(|selection| selection.text);
        Some(crate::bench::ui_fixture::CompletionUiSnapshot {
            input: input.value().to_string(),
            cursor: input.cursor(),
            focused: input.focus_handle().is_focused(window),
            suggestions_open: self.reply_autocomplete.state.is_open,
            suggestion_count: self.reply_autocomplete.suggestions.len(),
            selected_text,
        })
    }

    fn clear_reply_input_if_needed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.pending_clear_reply_input {
            return;
        }

        self.pending_clear_reply_input = false;
        if let Some(input) = self.reply_input.clone() {
            input.update(cx, |state, cx| state.set_value("", window, cx));
        }
    }

    fn focus_reply_input_if_needed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.focus_reply_input_on_open {
            return;
        }

        self.focus_reply_input_on_open = false;
        if let Some(input) = self.reply_input.as_ref() {
            let focus_handle = input.read(cx).focus_handle().clone();
            window.focus(&focus_handle, cx);
        }
    }

    fn start_refresh_waker(&mut self, cx: &mut Context<Self>) {
        if self.refresh_waker_running || !self.state.is_open {
            return;
        }

        self.refresh_waker_running = true;
        cx.spawn(async move |this, cx| {
            for _ in 0..THREAD_VIEWER_REFRESH_TICKS {
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(
                        THREAD_VIEWER_REFRESH_INTERVAL_MS,
                    ))
                    .await;

                let keep_running = this
                    .update(cx, |viewer, cx| {
                        if !viewer.state.is_open {
                            viewer.refresh_waker_running = false;
                            return false;
                        }

                        if viewer.refresh_gate == Some(Self::refresh_gate_key()) {
                            return true;
                        }

                        let before = std::mem::take(&mut viewer.thread_messages);
                        viewer.refresh();
                        if Self::thread_messages_changed(&before, &viewer.thread_messages) {
                            crate::backend::metrics::record_ui_notify_source(
                                "thread_viewer.refresh_waker",
                            );
                            cx.notify();
                        }
                        true
                    })
                    .unwrap_or(false);

                if !keep_running {
                    return;
                }
            }

            let _ = this.update(cx, |viewer, _cx| {
                viewer.refresh_waker_running = false;
            });
        })
        .detach();
    }

    /// Send reply in thread context
    fn send_reply(&mut self, cx: &mut Context<Self>) {
        let Some(reply_input) = self.reply_input.as_ref() else {
            return;
        };
        let reply_text = reply_input.read(cx).value().trim().to_string();
        if reply_text.is_empty() {
            return;
        }
        if let Err(block) = backend::irc::ensure_authenticated_send_ready() {
            push_thread_send_blocked_toast(&block.to_string());
            return;
        }
        // The input is cleared as soon as the reply is handed off, so anything
        // known to make Twitch reject it is checked while the text is still
        // there to edit.
        let reply_chars =
            unicode_segmentation::UnicodeSegmentation::graphemes(reply_text.as_str(), true).count();
        if reply_chars > crate::config::TWITCH_MESSAGE_CHAR_LIMIT {
            push_thread_send_blocked_toast(&format!(
                "Message too long: {reply_chars} / {} characters",
                crate::config::TWITCH_MESSAGE_CHAR_LIMIT
            ));
            return;
        }
        if let (Some(channel), Some(msg_id)) = (&self.state.channel, &self.state.root_message_id) {
            if let Err(block) = backend::irc::ensure_channel_send_ready(channel) {
                push_thread_send_blocked_toast(block.trim());
                return;
            }
            let root_user = self
                .thread_messages
                .iter()
                .find(|m| m.id.as_ref() == msg_id.as_str())
                .map(|m| m.sender.clone())
                .unwrap_or_default();

            if root_user.is_empty() {
                push_thread_send_blocked_toast("The message this thread replies to is gone.");
            } else {
                backend::send_reply_async(
                    channel.to_string(),
                    reply_text,
                    msg_id.to_string(),
                    root_user.to_string(),
                );
                self.close_reply_autocomplete();
                self.tab_completion = None;
                self.pending_clear_reply_input = true;
                self.refresh();
                cx.notify();
            }
        }
    }
}

impl Render for ThreadViewer {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.state.is_open {
            return div().into_any_element();
        }

        self.ensure_reply_input(window, cx);
        self.clear_reply_input_if_needed(window, cx);
        self.focus_reply_input_if_needed(window, cx);
        self.start_refresh_waker(cx);

        let viewport = window.viewport_size();
        let viewport_width: f32 = viewport.width.into();
        let viewport_height: f32 = viewport.height.into();
        let viewer_fit = crate::utils::fit_overlay_to_viewport(
            viewport,
            self.state.width,
            self.state.height,
            THREAD_VIEWER_WINDOW_MARGIN,
        );
        let width = viewer_fit.width;
        let height = viewer_fit.height;
        self.state.x = crate::utils::clamp_overlay_axis(
            self.state.x,
            width,
            viewport_width,
            viewer_fit.margin_x,
        );
        self.state.y = crate::utils::clamp_overlay_axis(
            self.state.y,
            height,
            viewport_height,
            viewer_fit.margin_y,
        );

        div()
            .id("thread-viewer")
            .absolute()
            .left(px(self.state.x))
            .top(px(self.state.y))
            .w(px(width))
            .h(px(height))
            .bg(colors::bg_elevated())
            .rounded(Radii::lg())
            .border_1()
            .border_color(colors::border())
            .shadow_lg()
            .flex()
            .flex_col()
            .overflow_hidden()
            .block_overlay_pointer_passthrough()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                let input_focused = this
                    .reply_input
                    .as_ref()
                    .is_some_and(|input| input.read(cx).focus_handle().is_focused(window));

                if input_focused {
                    let key = event.keystroke.key.as_str();
                    let plain_tab = key == "tab"
                        && !event.keystroke.modifiers.control
                        && !event.keystroke.modifiers.platform
                        && !event.keystroke.modifiers.alt;
                    if key == "escape" {
                        if this.reply_autocomplete.state.is_open {
                            this.close_reply_autocomplete();
                            cx.stop_propagation();
                            cx.notify();
                            return;
                        }
                        if this.tab_completion.take().is_some() {
                            cx.stop_propagation();
                            cx.notify();
                            return;
                        }
                    }

                    if this.reply_autocomplete.state.is_open {
                        match key {
                            "up" => {
                                this.move_reply_autocomplete_selection(-1);
                                cx.stop_propagation();
                                cx.notify();
                                return;
                            }
                            "down" => {
                                this.move_reply_autocomplete_selection(1);
                                cx.stop_propagation();
                                cx.notify();
                                return;
                            }
                            "enter" => {
                                let index = this.reply_autocomplete.state.selected_index;
                                if this.reply_autocomplete.suggestions.is_empty() {
                                    this.close_reply_autocomplete();
                                } else {
                                    this.accept_reply_autocomplete(index, window, cx);
                                }
                                cx.stop_propagation();
                                cx.notify();
                                return;
                            }
                            "tab" if plain_tab => {
                                let index = this.reply_autocomplete.state.selected_index;
                                if this.reply_autocomplete.suggestions.is_empty() {
                                    this.close_reply_autocomplete();
                                } else {
                                    this.accept_reply_autocomplete(index, window, cx);
                                }
                                cx.stop_propagation();
                                cx.notify();
                                return;
                            }
                            _ => {}
                        }
                    }

                    if plain_tab {
                        this.cycle_inline_tab_completion(
                            event.keystroke.modifiers.shift,
                            window,
                            cx,
                        );
                        cx.stop_propagation();
                        return;
                    }

                    this.tab_completion = None;
                    if matches!(key, "left" | "right" | "home" | "end") {
                        let viewer = cx.entity().clone();
                        cx.defer(move |cx| {
                            viewer.update(cx, |viewer, cx| {
                                viewer.refresh_reply_autocomplete_from_input(cx);
                                cx.notify();
                            });
                        });
                    }
                }

                if event.keystroke.key == "escape" {
                    this.state.close();
                    cx.notify();
                }
            }))
            .child(self.render_header(cx))
            .child(self.render_messages(cx))
            .child(self.render_reply_input(cx))
            .into_any_element()
    }
}

impl ThreadViewer {
    fn render_header(&mut self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let channel = self.state.channel.clone().unwrap_or_default();
        let msg_count = self.thread_messages.len();

        div()
            .h(px(40.0))
            .px_3()
            .bg(colors::bg_plate())
            .border_b_1()
            .border_color(colors::border())
            .flex()
            .items_center()
            .justify_between()
            .cursor_grab()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &MouseDownEvent, _window, _cx| {
                    this.drag_mode = Some(DragMode::Move);
                    this.drag_start = event.position;
                    this.initial_pos = (this.state.x, this.state.y);
                }),
            )
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _window, _cx| {
                    this.drag_mode = None;
                }),
            )
            .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _window, cx| {
                if this.drag_mode == Some(DragMode::Move) {
                    let delta_x: f32 = event.position.x.into();
                    let delta_y: f32 = event.position.y.into();
                    let start_x: f32 = this.drag_start.x.into();
                    let start_y: f32 = this.drag_start.y.into();
                    this.state.x = this.initial_pos.0 + delta_x - start_x;
                    this.state.y = this.initial_pos.1 + delta_y - start_y;
                    cx.notify();
                }
            }))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .text_sm()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(colors::text_primary())
                            .child("Thread"),
                    )
                    .child(
                        div()
                            .px_2()
                            .py_0p5()
                            .rounded(Radii::xl())
                            .bg(colors::bg_hover())
                            .text_xs()
                            .text_color(colors::text_secondary())
                            .child(format!("{} messages", msg_count)),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(colors::text_muted())
                            .child(channel.to_string()),
                    ),
            )
            .child(
                div()
                    .id("thread-close")
                    .role(Role::Button)
                    .aria_label("Close thread")
                    .on_hover(crate::components::overlays::text_tooltip_on_hover(
                        "thread-close".to_string(),
                        "Close thread".to_string(),
                    ))
                    .size(px(24.0))
                    .rounded(Radii::sm())
                    .cursor_pointer()
                    .hover(|s| s.bg(colors::bg_hover()))
                    .flex()
                    .items_center()
                    .justify_center()
                    .on_activate(cx.listener(|this, _, _window, cx| {
                        this.state.close();
                        cx.notify();
                    }))
                    .child(
                        div()
                            .text_sm()
                            .text_color(colors::text_secondary())
                            .child(crate::icons::CLOSE_GLYPH),
                    ),
            )
    }

    fn render_messages(&self, _cx: &mut Context<Self>) -> impl IntoElement + use<> {
        if self.thread_messages.is_empty() {
            return div()
                .flex_1()
                .flex()
                .items_center()
                .justify_center()
                .child(
                    div()
                        .text_sm()
                        .text_color(colors::text_muted())
                        .child("No messages in thread"),
                )
                .into_any_element();
        }

        let root_msg_id = self.state.root_message_id.clone().unwrap_or_default();

        div()
            .id("thread-messages")
            .flex_1()
            .overflow_y_scroll()
            .px_2()
            .py_2()
            .children(self.thread_messages.iter().enumerate().map(|(idx, msg)| {
                let is_root = msg.id.as_ref() == root_msg_id.as_str();
                render_thread_message(idx, msg, is_root)
            }))
            .into_any_element()
    }

    fn render_reply_input(&mut self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        self.reply_autocomplete.refresh_open_emote_suggestions();
        self.reply_autocomplete
            .suggestions
            .truncate(THREAD_COMPLETION_MAX_ROWS);
        if self.reply_autocomplete.state.selected_index >= self.reply_autocomplete.suggestions.len()
        {
            self.reply_autocomplete.state.selected_index = 0;
        }
        let completion_bar = self.render_reply_autocomplete(cx);
        let reply_input = self.reply_input.clone();
        let send_block_reason = backend::irc::ensure_authenticated_send_ready()
            .err()
            .map(|block| thread_send_block_status_text(&block));
        let send_blocked = send_block_reason.is_some();

        div()
            .h(px(THREAD_COMPOSER_HEIGHT))
            .relative()
            .bg(colors::bg_elevated())
            .border_t_1()
            .border_color(colors::border_subtle())
            .flex()
            .items_center()
            .text_sm()
            .text_color(colors::text_primary())
            .child(completion_bar)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .flex()
                    .items_center()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _, _window, cx| {
                            this.tab_completion = None;
                            let viewer = cx.entity().clone();
                            cx.defer(move |cx| {
                                viewer.update(cx, |viewer, cx| {
                                    viewer.refresh_reply_autocomplete_from_input(cx);
                                    cx.notify();
                                });
                            });
                        }),
                    )
                    .on_mouse_up(
                        MouseButton::Left,
                        cx.listener(|this, _, _window, cx| {
                            this.refresh_reply_autocomplete_from_input(cx);
                        }),
                    )
                    .on_mouse_up_out(
                        MouseButton::Left,
                        cx.listener(|this, _, _window, cx| {
                            this.refresh_reply_autocomplete_from_input(cx);
                        }),
                    )
                    .when_some(reply_input, |field, input| {
                        field.child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .h_full()
                                .flex()
                                .items_center()
                                .key_context(THREAD_REPLY_INPUT_CONTEXT)
                                .on_action(cx.listener(|this, action: &TabComplete, window, cx| {
                                    this.handle_reply_tab_complete_action(
                                        action.reverse,
                                        window,
                                        cx,
                                    );
                                    cx.stop_propagation();
                                }))
                                .child(
                                    Input::new(&input)
                                        .appearance(false)
                                        .bordered(false)
                                        .focus_bordered(false),
                                ),
                        )
                    }),
            )
            .when_some(send_block_reason, |row, reason| {
                row.child(
                    div()
                        .max_w(px(150.0))
                        .text_xs()
                        .text_color(colors::text_muted())
                        .truncate()
                        .child(reason),
                )
            })
            .child(
                div()
                    .id("thread-send")
                    .w(px(THREAD_SEND_RAIL_WIDTH))
                    .h_full()
                    .flex_shrink_0()
                    .bg(if send_blocked {
                        colors::bg_elevated()
                    } else {
                        colors::accent_transparent(0.16)
                    })
                    .border_l_1()
                    .border_color(colors::border_subtle())
                    .cursor_pointer()
                    .flex()
                    .items_center()
                    .justify_center()
                    .hover(move |s| {
                        if send_blocked {
                            s.bg(colors::bg_hover())
                        } else {
                            s.bg(colors::accent_transparent(0.22))
                        }
                    })
                    .on_activate(cx.listener(|this, _, _window, cx| {
                        this.send_reply(cx);
                    }))
                    .child(
                        div()
                            .text_sm()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(if send_blocked {
                                colors::text_muted()
                            } else {
                                colors::accent_hover()
                            })
                            .child("Send"),
                    ),
            )
    }

    fn render_reply_autocomplete(&mut self, cx: &mut Context<Self>) -> AnyElement {
        if !self.reply_autocomplete.state.is_open {
            return div().into_any_element();
        }

        let suggestions = self.reply_autocomplete.suggestions.clone();
        let selected_index = self.reply_autocomplete.state.selected_index;
        let completion_type = self.reply_autocomplete.state.ac_type;
        let visible_rows = suggestions.len().clamp(1, THREAD_COMPLETION_MAX_ROWS);
        let height = visible_rows as f32 * THREAD_COMPLETION_ROW_HEIGHT;

        let mut bar = div()
            .id("thread-reply-autocomplete")
            .absolute()
            .left_0()
            .right(px(THREAD_SEND_RAIL_WIDTH))
            .bottom(px(THREAD_COMPOSER_HEIGHT - 1.0))
            .max_h(px(height))
            .bg(colors::bg_elevated())
            .border_1()
            .border_color(colors::border())
            .rounded_t(Radii::md())
            .shadow_lg()
            .overflow_y_scroll()
            .overflow_x_hidden()
            .occlude()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_mouse_down(MouseButton::Right, |_, _, cx| cx.stop_propagation())
            .on_scroll(|_, _, cx| cx.stop_propagation());

        if suggestions.is_empty() {
            let status = match completion_type {
                AutocompleteType::Emote => "No matching emotes",
                AutocompleteType::Mention => "No matching recent chatters",
                AutocompleteType::Tab => "No matching emotes or chatters",
                AutocompleteType::Command => "No matching commands",
            };
            bar = bar.child(
                div()
                    .h(px(THREAD_COMPLETION_ROW_HEIGHT))
                    .px_3()
                    .flex()
                    .items_center()
                    .text_xs()
                    .text_color(colors::text_muted())
                    .child(status),
            );
        } else {
            bar = bar.children(
                suggestions
                    .into_iter()
                    .enumerate()
                    .map(|(index, suggestion)| {
                        self.render_reply_autocomplete_row(
                            suggestion,
                            index,
                            index == selected_index,
                            cx,
                        )
                    }),
            );
        }

        bar.into_any_element()
    }

    fn render_reply_autocomplete_row(
        &self,
        suggestion: AutocompleteSuggestion,
        index: usize,
        selected: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let text = suggestion.text.clone();
        let image_url = suggestion.image_url.clone();
        let provider = suggestion.provider.clone();
        let is_mention = suggestion.emote_type.is_none() && suggestion.text.starts_with('@');

        div()
            .id(("thread-reply-suggestion", index))
            .h(px(THREAD_COMPLETION_ROW_HEIGHT))
            .px_2()
            .flex()
            .items_center()
            .gap_2()
            .cursor_pointer()
            .when(selected, |row| row.bg(colors::accent_transparent(0.15)))
            .hover(|row| row.bg(colors::bg_hover()))
            .on_activate(cx.listener(move |this, _, window, cx| {
                this.accept_reply_autocomplete(index, window, cx);
                cx.stop_propagation();
            }))
            .child(if let Some(image_url) = image_url {
                img(SharedString::from(image_url))
                    .size(px(THREAD_COMPLETION_IMAGE_SIZE))
                    .object_fit(ObjectFit::Contain)
                    .into_any_element()
            } else {
                div()
                    .size(px(THREAD_COMPLETION_IMAGE_SIZE))
                    .rounded(Radii::md())
                    .bg(colors::accent_transparent(0.08))
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_sm()
                    .text_color(colors::text_secondary())
                    .child(if is_mention { "@" } else { ":" })
                    .into_any_element()
            })
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_sm()
                    .text_color(colors::text_primary())
                    .truncate()
                    .child(text),
            )
            .when_some(provider, |row, provider| {
                row.child(
                    div()
                        .text_xs()
                        .text_color(colors::text_muted())
                        .child(provider),
                )
            })
            .into_any_element()
    }
}

fn push_thread_send_blocked_toast(error: &str) {
    backend::push_ui_action(backend::UiAction::ShowWarningToast {
        message: "Reply not sent".to_string(),
        detail: Some(error.trim().to_string()),
    });
}

fn thread_send_block_status_text(block: &backend::irc::SendBlock) -> &'static str {
    use backend::irc::SendBlock;
    match block {
        SendBlock::Anonymous => "Anonymous chat active",
        SendBlock::NeedsReauth | SendBlock::NoChatToken { .. } => "Re-authentication required",
        SendBlock::NoAccount => "Log in to Twitch",
        SendBlock::Reconnecting | SendBlock::Disconnected | SendBlock::ClientNotReady => {
            "Reply unavailable"
        }
    }
}

/// Render a single thread message
fn render_thread_message(idx: usize, msg: &ChatMessage, is_root: bool) -> impl IntoElement + use<> {
    let user_color = msg
        .color
        .as_deref()
        .and_then(crate::utils::color::parse_user_color_opt)
        .unwrap_or(colors::accent());
    let row_group = SharedString::from(format!("thread-msg-group-{idx}"));
    let is_whisper = matches!(
        msg.flags.highlight_type.as_ref(),
        Some(crate::backend::HighlightType::Whisper)
    );
    let menu_target = MessageMenuTarget {
        message_id: msg.id.to_string(),
        content: msg.content.to_string(),
        sender: msg.sender.to_string(),
        display_name: msg.display_name.to_string(),
        timestamp: msg.timestamp,
        channel: msg.channel.to_string(),
        source_split_id: None,
        allow_remote_actions: msg.allows_remote_message_actions() && !is_whisper,
        is_whisper,
        highlight_type: msg.flags.highlight_type.clone(),
    };
    let card_login = msg.sender.to_string();
    let card_display_name = msg.display_name.to_string();
    let card_user_id = msg.user_id.as_ref().map(ToString::to_string);
    let card_channel = msg.channel.to_string();
    let card_color = msg.color.as_ref().map(ToString::to_string);
    let card_badges: Vec<String> = msg.badges.iter().map(ToString::to_string).collect();
    let jump_channel = msg.channel.to_string();
    let jump_message_id = msg.id.to_string();
    let can_jump = !msg.flags.is_system;
    let deleted_action = crate::backend::deleted_message_display::action_for_deleted_message_mode(
        crate::backend::settings::get_deleted_message_mode(),
    );
    let is_deleted = msg.flags.is_deleted;
    let show_deleted_chip = is_deleted
        && matches!(
            deleted_action,
            crate::backend::deleted_message_display::DeletedMessageDisplayAction::MarkDeleted
        );
    let content_color = if show_deleted_chip {
        colors::text_muted()
    } else {
        colors::text_primary()
    };
    let content_slots = if is_deleted
        && matches!(
            deleted_action,
            crate::backend::deleted_message_display::DeletedMessageDisplayAction::ReplaceWithPlaceholder
        ) {
        vec![crate::backend::slots::MessageSlot::text(
            crate::backend::deleted_message_display::DELETED_MESSAGE_PLACEHOLDER,
        )]
    } else {
        msg.slots.to_vec()
    };
    let timestamp = crate::backend::settings::should_show_message_timestamps(
        crate::backend::bridge::get_cached_channel_live_status(&msg.channel),
    )
    .then(|| format_message_timestamp(msg.timestamp))
    .flatten();

    div()
        .id(SharedString::from(format!("thread-msg-{}", idx)))
        .group(row_group.clone())
        .w_full()
        .px_2()
        .py_1p5()
        .rounded(Radii::sm())
        .on_mouse_down(MouseButton::Right, move |event, _window, cx| {
            let menu = ContextMenuState::open_message(
                event.position.x.into(),
                event.position.y.into(),
                menu_target.clone(),
            );
            backend::push_ui_action(backend::UiAction::ShowContextMenu(menu));
            cx.stop_propagation();
        })
        .when(is_root, |el| {
            el.bg(colors::accent().opacity(0.1))
                .border_l_2()
                .border_color(colors::accent())
        })
        .hover(|s| s.bg(colors::bg_hover()))
        .flex()
        .flex_col()
        .gap_1()
        .when_some(msg.reply.as_ref(), |el, reply| {
            el.child(
                div()
                    .text_xs()
                    .text_color(colors::text_muted())
                    .child(format!("↩ Replying to @{}", reply.parent_display_name)),
            )
        })
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .id(SharedString::from(format!("thread-msg-user-{idx}")))
                        .text_sm()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(user_color)
                        .cursor_pointer()
                        .hover(|style| style.underline())
                        .on_activate(move |event, _window, cx| {
                            backend::push_ui_action(backend::UiAction::ShowUserCard {
                                username: card_login.clone(),
                                display_name: card_display_name.clone(),
                                user_id: card_user_id.clone(),
                                channel: card_channel.clone(),
                                color: card_color.clone(),
                                badges: card_badges.clone(),
                                x: event.position().x.into(),
                                y: event.position().y.into(),
                            });
                            cx.stop_propagation();
                        })
                        .child(msg.display_name.to_string()),
                )
                .when_some(timestamp, |row, timestamp| {
                    row.child(
                        div()
                            .text_xs()
                            .text_color(colors::text_muted())
                            .child(timestamp),
                    )
                })
                .when(show_deleted_chip, |row| {
                    row.child(
                        div()
                            .px_1p5()
                            .py_0p5()
                            .rounded(Radii::full())
                            .bg(colors::bg_hover())
                            .text_xs()
                            .text_color(colors::text_muted())
                            .child("Deleted"),
                    )
                })
                .when(can_jump, |row| {
                    row.child(
                        div()
                            .id(SharedString::from(format!("thread-msg-jump-{idx}")))
                            .ml_auto()
                            .px_1()
                            .rounded(Radii::sm())
                            .text_xs()
                            .text_color(colors::text_muted().opacity(0.0))
                            .group_hover(row_group, |style| style.text_color(colors::text_muted()))
                            .hover(|style| style.bg(colors::bg_hover()))
                            .cursor_pointer()
                            .on_activate(move |_event, _window, cx| {
                                backend::push_ui_action(backend::UiAction::JumpToMessage {
                                    channel: jump_channel.clone(),
                                    message_id: jump_message_id.clone(),
                                });
                                cx.stop_propagation();
                            })
                            .child("Jump to message"),
                    )
                }),
        )
        .child(render_slot_content(
            &content_slots,
            &msg.channel,
            content_color,
        ))
}

/// Render message content with proper slot rendering (emotes, mentions, links)
fn render_slot_content(
    slots: &[crate::backend::slots::MessageSlot],
    channel: &str,
    text_color: Hsla,
) -> impl IntoElement + use<> {
    use crate::backend::irc::get_user_color;
    use crate::backend::slots::resolver::{ResolvedSlot, SlotResolver};
    use crate::components::chat::canvas_renderer::layout::get_cached_color;

    const EMOTE_SIZE: f32 = 20.0;

    let resolver = SlotResolver::new(channel);
    let display_rules = backend::settings::emote_display_rules_snapshot();
    let mut content = div().flex().flex_wrap().items_center().gap_0().text_sm();

    for (slot_idx, slot) in slots.iter().enumerate() {
        let resolved = resolver.resolve(slot);
        match resolved {
            ResolvedSlot::TwitchGif { alt_text, .. } => {
                content = content.child(div().text_color(text_color).child(alt_text.to_string()));
            }
            ResolvedSlot::Text(text) => {
                for fragment in split_preserving_whitespace(&text) {
                    content = content.child(div().text_color(text_color).child(fragment));
                }
            }
            ResolvedSlot::Emote(emote) => {
                let flags = display_rules
                    .as_deref()
                    .map(|rules| rules.flags_for(emote.provider, &emote.id, channel))
                    .unwrap_or_default();
                if flags.show_as_text {
                    // The parser drops the space before an emote; restore it
                    // unless the code was typed glued to punctuation.
                    let code = slot.emote_source_text().unwrap_or(&emote.name);
                    let spaced = slot_idx > 0
                        && !echo_core::slot_parser::emote_slot_is_glued(slots, slot_idx)
                        && !matches!(
                            &slots[slot_idx - 1],
                            crate::backend::slots::MessageSlot::Text(text)
                                if text.ends_with(char::is_whitespace)
                        );
                    let code = if spaced {
                        format!(" {code}")
                    } else {
                        code.to_string()
                    };
                    content = content.child(div().text_color(text_color).child(code));
                    continue;
                }

                let url = emote
                    .url_2x
                    .as_ref()
                    .or(emote.url_1x.as_ref())
                    .unwrap_or(&emote.url);

                content = content.child(
                    if flags.still_image {
                        // GPUI's `img(url)` would play the animation.
                        img(crate::components::ui::still_frame_source(
                            SharedString::from(url.to_string()),
                            crate::cache::loader::PreloadSource::VisibleChat,
                        ))
                    } else {
                        img(url.to_string())
                    }
                    .w(px(EMOTE_SIZE))
                    .h(px(EMOTE_SIZE))
                    .object_fit(ObjectFit::Contain),
                );
            }
            ResolvedSlot::Url { url, .. } => {
                let open_url = url.to_string();
                content = content.child(
                    div()
                        .id(("thread-link", slot_idx))
                        .text_color(colors::link())
                        .cursor_pointer()
                        .hover(|s| s.underline())
                        .on_activate(move |_event, _window, _cx| {
                            backend::push_ui_action(backend::UiAction::ConfirmOpenLink {
                                url: open_url.clone(),
                                mode: backend::ui_actions::LinkOpenMode::Configured,
                            });
                        })
                        .child(url.to_string()),
                );
            }
            ResolvedSlot::Mention(username) => {
                let mention_color = get_user_color(&username)
                    .map(|hex| get_cached_color(&hex))
                    .unwrap_or(colors::accent());

                content = content.child(
                    div()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(mention_color)
                        .child(format!("@{}", username)),
                );
            }
            ResolvedSlot::Emoji { ref url, .. } => {
                content = content.child(
                    img(url.to_string())
                        .w(px(EMOTE_SIZE))
                        .h(px(EMOTE_SIZE))
                        .object_fit(ObjectFit::Contain),
                );
            }
        }
    }

    content
}

fn split_preserving_whitespace(text: &str) -> Vec<String> {
    let mut fragments = Vec::new();
    let mut current = String::new();
    let mut in_whitespace = None;

    for ch in text.chars() {
        let is_whitespace = ch.is_whitespace();
        match in_whitespace {
            Some(state) if state != is_whitespace => {
                fragments.push(std::mem::take(&mut current));
                current.push(if ch == '\n' { ' ' } else { ch });
                in_whitespace = Some(is_whitespace);
            }
            Some(_) => {
                current.push(if ch == '\n' { ' ' } else { ch });
            }
            None => {
                current.push(if ch == '\n' { ' ' } else { ch });
                in_whitespace = Some(is_whitespace);
            }
        }
    }

    if !current.is_empty() {
        fragments.push(current);
    }

    if fragments.is_empty() {
        fragments.push(String::new());
    }

    fragments
}
