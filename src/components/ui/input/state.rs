//! InputState - Core state management for text input
//!
//! Manages text content, cursor position, selection, and undo/redo history.

#![allow(clippy::type_complexity)]

use gpui::{
    App, Bounds, CaretAffinity, ClipboardItem, Context, EntityInputHandler, EventEmitter,
    FocusHandle, Focusable, IntoElement, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels,
    Point, Render, ShapedLine, SharedString, Subscription, TextRun, UTF16Selection,
    WeakFocusHandle, Window, div, px, size,
};
use std::ops::Range;
use unicode_segmentation::UnicodeSegmentation;

thread_local! {
    static INPUT_FOCUS_HANDLES: std::cell::RefCell<Vec<WeakFocusHandle>> = const { std::cell::RefCell::new(Vec::new()) };
}

pub(crate) fn is_input_focused(window: &Window) -> bool {
    INPUT_FOCUS_HANDLES.with(|handles| {
        let mut handles = handles.borrow_mut();
        handles.retain(|handle| handle.upgrade().is_some());
        handles
            .iter()
            .filter_map(WeakFocusHandle::upgrade)
            .any(|handle| handle.is_focused(window))
    })
}

use super::{
    Backspace, Copy, Cut, Delete, DeleteWord, DeleteWordForward, Enter, Escape, MoveLeft,
    MoveRight, MoveToEnd, MoveToStart, MoveWordLeft, MoveWordRight, Paste, Redo, SelectAll,
    SelectLeft, SelectRight, SelectToEnd, SelectToStart, SelectWordLeft, SelectWordRight, Undo,
};

/// Events emitted by InputState
#[derive(Clone, Debug)]
pub enum InputEvent {
    /// Text content changed
    Change,
    /// Enter key pressed
    PressEnter,
    /// Input gained focus
    Focus,
    /// Input lost focus
    Blur,
}

/// Undo/redo history entry
#[derive(Clone)]
struct HistoryEntry {
    text: String,
    cursor: usize,
    caret_affinity: CaretAffinity,
    selection: Option<Range<usize>>,
}

/// The core state for a text input field
pub struct InputState {
    /// Focus handle for this input
    focus_handle: FocusHandle,
    /// The text content
    text: String,
    /// Cursor position (byte offset)
    cursor: usize,
    /// Visual side of the logical cursor boundary. Pointer hit-testing retains
    /// this identity at mixed-direction boundaries; all other cursor changes
    /// use the canonical leading/trailing policy.
    caret_affinity: CaretAffinity,
    /// Selection range (byte offsets), None if no selection
    selection: Option<Range<usize>>,
    /// IME marked text range
    ime_marked_range: Option<Range<usize>>,
    /// State before the current IME composition began. Marked-text updates
    /// replace each other, so they share one undo entry when committed.
    ime_composition_start: Option<HistoryEntry>,
    /// Placeholder text
    placeholder: SharedString,
    /// Whether the input is disabled
    disabled: bool,
    /// Undo history
    undo_stack: Vec<HistoryEntry>,
    /// Redo history
    redo_stack: Vec<HistoryEntry>,
    /// Subscriptions
    _subscriptions: Vec<Subscription>,
    /// Exact single-line glyph layout and canvas bounds published by Input's
    /// prepaint pass for click/drag cursor hit-testing.
    pointer_bounds: Option<Bounds<Pixels>>,
    pointer_layout: Option<ShapedLine>,
    pointer_text: Option<String>,
    pointer_text_run: Option<TextRun>,
    pointer_font_size: Option<Pixels>,
    pointer_line_height: Option<Pixels>,
    pointer_selection_anchor: Option<usize>,
    /// How far the text is scrolled left so the caret stays inside the box.
    scroll_x: Pixels,

    /// Ranges of misspelled words (byte offsets)
    pub(crate) spell_check_ranges: Vec<Range<usize>>,
}

impl EventEmitter<InputEvent> for InputState {}

/// Normalize text for Echo's single-line input control.
///
/// `Input` is painted with GPUI's single-line shaper, so every path that
/// supplies text (including programmatic values and IME commits) must keep
/// line separators out of its state.  A separator becomes a space rather than
/// being removed, retaining word boundaries from pasted or imported content.
pub(crate) fn normalize_single_line_text(value: &str) -> String {
    if !value.chars().any(|character| {
        matches!(
            character,
            '\r' | '\n' | '\u{000B}' | '\u{000C}' | '\u{0085}' | '\u{2028}' | '\u{2029}'
        )
    }) {
        return value.to_string();
    }

    let mut normalized = String::with_capacity(value.len());
    let mut characters = value.chars().peekable();
    while let Some(character) = characters.next() {
        match character {
            '\r' if characters.peek() == Some(&'\n') => {
                characters.next();
                normalized.push(' ');
            }
            '\r' | '\n' | '\u{000B}' | '\u{000C}' | '\u{0085}' | '\u{2028}' | '\u{2029}' => {
                normalized.push(' ');
            }
            _ => normalized.push(character),
        }
    }
    normalized
}

impl InputState {
    /// Create a new InputState
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus_handle = cx.focus_handle();
        INPUT_FOCUS_HANDLES.with(|handles| {
            let mut handles = handles.borrow_mut();
            handles.retain(|handle| handle.upgrade().is_some());
            handles.push(focus_handle.downgrade());
        });

        let _subscriptions = vec![
            cx.on_focus(&focus_handle, window, Self::on_focus),
            cx.on_blur(&focus_handle, window, Self::on_blur),
        ];

        Self {
            focus_handle,
            text: String::new(),
            cursor: 0,
            caret_affinity: CaretAffinity::Trailing,
            selection: None,
            ime_marked_range: None,
            ime_composition_start: None,
            placeholder: SharedString::default(),
            disabled: false,
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            _subscriptions,
            pointer_bounds: None,
            pointer_layout: None,
            pointer_text: None,
            pointer_text_run: None,
            pointer_font_size: None,
            pointer_line_height: None,
            pointer_selection_anchor: None,
            scroll_x: px(0.0),
            spell_check_ranges: Vec::new(),
        }
    }

    /// Set placeholder text (builder pattern)
    pub fn with_placeholder(mut self, placeholder: impl Into<SharedString>) -> Self {
        self.placeholder = placeholder.into();
        self
    }

    /// Seed a controlled input before it is first rendered without creating an
    /// undo entry or emitting a change event.
    pub fn with_value(mut self, value: impl Into<SharedString>) -> Self {
        let value: SharedString = value.into();
        self.text = normalize_single_line_text(&value);
        self.set_cursor_canonical(self.text.len());
        self
    }

    /// Get the current text value
    pub fn value(&self) -> &str {
        &self.text
    }

    /// Get cursor position (byte offset)
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    pub(crate) fn cursor_affinity(&self) -> CaretAffinity {
        self.caret_affinity
    }

    /// Get selection range
    pub fn selection(&self) -> Option<Range<usize>> {
        self.selection.clone()
    }

    pub(crate) fn marked_range(&self) -> Option<Range<usize>> {
        self.valid_marked_range()
    }

    /// Scroll so a caret at `caret_x` stays inside `visible_width`, never past
    /// the end of a line `line_width` wide. Without a caret the start shows.
    pub(crate) fn follow_caret(
        &mut self,
        caret_x: Option<Pixels>,
        line_width: Pixels,
        visible_width: Pixels,
        caret_width: Pixels,
    ) -> Pixels {
        let max_scroll = (line_width + caret_width - visible_width).max(px(0.0));
        let scroll_x = match caret_x {
            Some(caret_x) if caret_x < self.scroll_x => caret_x,
            Some(caret_x) if caret_x + caret_width > self.scroll_x + visible_width => {
                caret_x + caret_width - visible_width
            }
            Some(_) => self.scroll_x,
            None => px(0.0),
        };
        self.scroll_x = scroll_x.clamp(px(0.0), max_scroll);
        self.scroll_x
    }

    /// Record the painted line for pointer hit-testing. `bounds` starts where
    /// the text starts, so it reaches left of the box by the current scroll.
    pub(crate) fn update_pointer_layout(
        &mut self,
        bounds: Bounds<Pixels>,
        layout: ShapedLine,
        text: &str,
        text_run: &TextRun,
        font_size: Pixels,
        line_height: Pixels,
    ) {
        self.pointer_bounds = Some(bounds);
        self.pointer_layout = Some(layout);
        self.pointer_text = Some(text.to_owned());
        self.pointer_text_run = Some(text_run.clone());
        self.pointer_font_size = Some(font_size);
        self.pointer_line_height = Some(line_height);
    }

    fn canonical_affinity_for_index(&self, index: usize) -> CaretAffinity {
        if index.min(self.text.len()) == self.text.len() {
            CaretAffinity::Trailing
        } else {
            CaretAffinity::Leading
        }
    }

    fn set_cursor_canonical(&mut self, cursor: usize) {
        self.cursor = cursor.min(self.text.len());
        self.caret_affinity = self.canonical_affinity_for_index(self.cursor);
    }

    fn caret_for_pointer_position(
        &self,
        position: Point<Pixels>,
    ) -> Option<(usize, CaretAffinity)> {
        if self.text.is_empty() {
            return Some((0, CaretAffinity::Trailing));
        }
        let (Some(bounds), Some(layout)) = (self.pointer_bounds, self.pointer_layout.as_ref())
        else {
            return None;
        };
        if layout.has_bidi_runs && !layout.bidi_geometry_complete {
            return None;
        }
        if position.y <= bounds.top() || position.x <= bounds.left() {
            return Some((0, self.canonical_affinity_for_index(0)));
        }
        if position.y >= bounds.bottom() || position.x >= bounds.right() {
            return Some((
                self.text.len(),
                self.canonical_affinity_for_index(self.text.len()),
            ));
        }
        let x = position.x - bounds.left();
        Some(layout.caret_for_x(x).map_or_else(
            || {
                let index = layout.closest_index_for_x(x).min(self.text.len());
                (index, self.canonical_affinity_for_index(index))
            },
            |caret| (caret.index.min(self.text.len()), caret.affinity),
        ))
    }

    fn set_pointer_selection(&mut self, anchor: usize, focus: (usize, CaretAffinity)) -> bool {
        let anchor = anchor.min(self.text.len());
        let (focus_index, caret_affinity) = focus;
        let focus_index = focus_index.min(self.text.len());
        let next_selection =
            (anchor != focus_index).then_some(anchor.min(focus_index)..anchor.max(focus_index));
        let changed = self.cursor != focus_index
            || self.caret_affinity != caret_affinity
            || self.selection != next_selection;
        self.cursor = focus_index;
        self.caret_affinity = caret_affinity;
        self.selection = next_selection;
        changed
    }

    pub(crate) fn handle_pointer_down(&mut self, event: &MouseDownEvent, cx: &mut Context<Self>) {
        if self.disabled {
            return;
        }
        self.commit_ime_composition();
        let Some(focus) = self.caret_for_pointer_position(event.position) else {
            return;
        };
        if event.click_count >= 2 {
            self.pointer_selection_anchor = None;
            if event.click_count >= 3 {
                self.select_all(cx);
            } else {
                let (position, _) = focus;
                self.select_word_at(position, cx);
            }
            return;
        }

        let anchor = if event.modifiers.shift {
            self.selection
                .as_ref()
                .map(|selection| {
                    if self.cursor == selection.start {
                        selection.end
                    } else {
                        selection.start
                    }
                })
                .unwrap_or(self.cursor)
        } else {
            focus.0
        };
        self.pointer_selection_anchor = Some(anchor);
        if self.set_pointer_selection(anchor, focus) {
            cx.notify();
        }
    }

    pub(crate) fn handle_pointer_move(&mut self, event: &MouseMoveEvent, cx: &mut Context<Self>) {
        if !event.dragging() {
            return;
        }
        let Some(anchor) = self.pointer_selection_anchor else {
            return;
        };
        let Some(focus) = self.caret_for_pointer_position(event.position) else {
            return;
        };
        if self.set_pointer_selection(anchor, focus) {
            cx.notify();
        }
    }

    pub(crate) fn handle_pointer_up(&mut self, _event: &MouseUpEvent, _cx: &mut Context<Self>) {
        self.pointer_selection_anchor = None;
    }

    /// Set the text value
    pub fn set_value(
        &mut self,
        value: impl Into<SharedString>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let value: SharedString = value.into();
        self.cancel_ime_composition();
        self.save_undo_state();
        self.text = normalize_single_line_text(&value);
        self.set_cursor_canonical(self.text.len());
        self.selection = None;
        self.redo_stack.clear();
        cx.emit(InputEvent::Change);
        cx.notify();
    }

    /// Insert text at cursor, replacing selection if any
    pub fn insert(&mut self, text: &str, _window: &mut Window, cx: &mut Context<Self>) {
        let text = normalize_single_line_text(text);
        self.commit_ime_composition();
        self.save_undo_state();

        if let Some(range) = self.selection.take() {
            self.text.replace_range(range.clone(), &text);
            self.set_cursor_canonical(range.start + text.len());
        } else {
            self.text.insert_str(self.cursor, &text);
            self.set_cursor_canonical(self.cursor + text.len());
        }

        self.redo_stack.clear();
        cx.emit(InputEvent::Change);
        cx.notify();
    }

    /// Deterministically seed the native no-send completion harness without
    /// synthesizing character input or adding a user-visible undo entry.
    pub(crate) fn set_value_for_completion_smoke(
        &mut self,
        value: &str,
        cx: &mut Context<Self>,
    ) -> bool {
        if !crate::benchmark_ui_fixture::is_enabled() {
            return false;
        }
        self.text.clear();
        self.text.push_str(&normalize_single_line_text(value));
        self.set_cursor_canonical(self.text.len());
        self.selection = None;
        self.ime_marked_range = None;
        self.ime_composition_start = None;
        self.undo_stack.clear();
        self.redo_stack.clear();
        cx.notify();
        true
    }

    /// Replace one stale-fenced UTF-8 fragment with a completion token.
    /// Existing surrounding whitespace is preserved, and the caret advances
    /// across existing whitespace just like the main chat composer.
    pub fn replace_exact_range_with_token(
        &mut self,
        range: Range<usize>,
        expected_fragment: &str,
        token: &str,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.ime_composition_start.is_some()
            || token.is_empty()
            || range.start > range.end
            || range.end > self.text.len()
            || !self.text.is_char_boundary(range.start)
            || !self.text.is_char_boundary(range.end)
            || self.text.get(range.clone()) != Some(expected_fragment)
        {
            return false;
        }

        let add_leading_space = self.text[..range.start]
            .chars()
            .next_back()
            .is_some_and(|ch| !ch.is_whitespace());
        let following_text = &self.text[range.end..];
        let add_trailing_space = following_text
            .chars()
            .next()
            .is_none_or(|ch| !ch.is_whitespace());
        let existing_whitespace_len = if add_trailing_space {
            0
        } else {
            following_text
                .char_indices()
                .take_while(|(_, ch)| ch.is_whitespace())
                .map(|(offset, ch)| offset + ch.len_utf8())
                .last()
                .unwrap_or(0)
        };

        let mut replacement = String::with_capacity(
            token.len() + usize::from(add_leading_space) + usize::from(add_trailing_space),
        );
        if add_leading_space {
            replacement.push(' ');
        }
        replacement.push_str(token);
        if add_trailing_space {
            replacement.push(' ');
        }

        self.save_undo_state();
        self.text.replace_range(range.clone(), &replacement);
        self.set_cursor_canonical(range.start + replacement.len() + existing_whitespace_len);
        self.selection = None;
        self.ime_marked_range = None;
        self.redo_stack.clear();
        cx.emit(InputEvent::Change);
        cx.notify();
        true
    }

    /// Delete selection or character before cursor
    fn backspace(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        if self.text.is_empty() {
            self.set_cursor_canonical(self.cursor);
            cx.notify();
            return;
        }

        self.commit_ime_composition();
        self.save_undo_state();

        if let Some(range) = self.selection.take() {
            self.text.replace_range(range.clone(), "");
            self.set_cursor_canonical(range.start);
        } else if self.cursor > 0 {
            let prev = self.prev_grapheme_boundary(self.cursor);
            self.text.replace_range(prev..self.cursor, "");
            self.set_cursor_canonical(prev);
        } else {
            self.set_cursor_canonical(self.cursor);
        }

        self.redo_stack.clear();
        cx.emit(InputEvent::Change);
        cx.notify();
    }

    /// Delete selection or character after cursor
    fn delete(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        if self.text.is_empty() {
            self.set_cursor_canonical(self.cursor);
            cx.notify();
            return;
        }

        self.commit_ime_composition();
        self.save_undo_state();

        if let Some(range) = self.selection.take() {
            self.text.replace_range(range.clone(), "");
            self.set_cursor_canonical(range.start);
        } else if self.cursor < self.text.len() {
            let next = self.next_grapheme_boundary(self.cursor);
            self.text.replace_range(self.cursor..next, "");
            self.set_cursor_canonical(self.cursor);
        } else {
            self.set_cursor_canonical(self.cursor);
        }

        self.redo_stack.clear();
        cx.emit(InputEvent::Change);
        cx.notify();
    }

    /// Delete word before cursor
    fn delete_word(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        if self.selection.is_none() && self.cursor == 0 {
            self.set_cursor_canonical(self.cursor);
            cx.notify();
            return;
        }

        self.commit_ime_composition();
        self.save_undo_state();

        if let Some(range) = self.selection.take() {
            self.text.replace_range(range.clone(), "");
            self.set_cursor_canonical(range.start);
        } else {
            let word_start = self.prev_word_boundary(self.cursor);
            self.text.replace_range(word_start..self.cursor, "");
            self.set_cursor_canonical(word_start);
        }

        self.redo_stack.clear();
        cx.emit(InputEvent::Change);
        cx.notify();
    }

    /// Delete word after cursor
    fn delete_word_forward(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        if self.selection.is_none() && self.cursor >= self.text.len() {
            self.set_cursor_canonical(self.cursor);
            cx.notify();
            return;
        }

        self.commit_ime_composition();
        self.save_undo_state();

        if let Some(range) = self.selection.take() {
            self.text.replace_range(range.clone(), "");
            self.set_cursor_canonical(range.start);
        } else {
            let word_end = self.next_word_boundary(self.cursor);
            self.text.replace_range(self.cursor..word_end, "");
            self.set_cursor_canonical(self.cursor);
        }

        self.redo_stack.clear();
        cx.emit(InputEvent::Change);
        cx.notify();
    }

    /// Move cursor left
    fn move_left(&mut self, cx: &mut Context<Self>) {
        self.commit_ime_composition();
        if let Some(range) = self.selection.take() {
            self.set_cursor_canonical(range.start);
        } else if self.cursor > 0 {
            self.set_cursor_canonical(self.prev_grapheme_boundary(self.cursor));
        } else {
            self.set_cursor_canonical(self.cursor);
        }
        cx.notify();
    }

    /// Move cursor right
    fn move_right(&mut self, cx: &mut Context<Self>) {
        self.commit_ime_composition();
        if let Some(range) = self.selection.take() {
            self.set_cursor_canonical(range.end);
        } else if self.cursor < self.text.len() {
            self.set_cursor_canonical(self.next_grapheme_boundary(self.cursor));
        } else {
            self.set_cursor_canonical(self.cursor);
        }
        cx.notify();
    }

    /// Move to start of text
    fn move_to_start(&mut self, cx: &mut Context<Self>) {
        self.commit_ime_composition();
        self.selection = None;
        self.set_cursor_canonical(0);
        cx.notify();
    }

    /// Move to end of text
    fn move_to_end(&mut self, cx: &mut Context<Self>) {
        self.commit_ime_composition();
        self.selection = None;
        self.set_cursor_canonical(self.text.len());
        cx.notify();
    }

    /// Move to previous word boundary
    fn move_word_left(&mut self, cx: &mut Context<Self>) {
        self.commit_ime_composition();
        self.selection = None;
        self.set_cursor_canonical(self.prev_word_boundary(self.cursor));
        cx.notify();
    }

    /// Move to next word boundary
    fn move_word_right(&mut self, cx: &mut Context<Self>) {
        self.commit_ime_composition();
        self.selection = None;
        self.set_cursor_canonical(self.next_word_boundary(self.cursor));
        cx.notify();
    }

    /// Extend selection left
    fn select_left(&mut self, cx: &mut Context<Self>) {
        self.commit_ime_composition();
        if self.cursor > 0 {
            let new_pos = self.prev_grapheme_boundary(self.cursor);
            self.extend_selection(new_pos);
        } else {
            self.set_cursor_canonical(self.cursor);
        }
        cx.notify();
    }

    /// Extend selection right
    fn select_right(&mut self, cx: &mut Context<Self>) {
        self.commit_ime_composition();
        if self.cursor < self.text.len() {
            let new_pos = self.next_grapheme_boundary(self.cursor);
            self.extend_selection(new_pos);
        } else {
            self.set_cursor_canonical(self.cursor);
        }
        cx.notify();
    }

    /// Select to start of text
    fn select_to_start(&mut self, cx: &mut Context<Self>) {
        self.commit_ime_composition();
        self.extend_selection(0);
        cx.notify();
    }

    /// Select to end of text
    fn select_to_end(&mut self, cx: &mut Context<Self>) {
        self.commit_ime_composition();
        self.extend_selection(self.text.len());
        cx.notify();
    }

    /// Select word left
    fn select_word_left(&mut self, cx: &mut Context<Self>) {
        self.commit_ime_composition();
        let new_pos = self.prev_word_boundary(self.cursor);
        self.extend_selection(new_pos);
        cx.notify();
    }

    /// Select word right
    fn select_word_right(&mut self, cx: &mut Context<Self>) {
        self.commit_ime_composition();
        let new_pos = self.next_word_boundary(self.cursor);
        self.extend_selection(new_pos);
        cx.notify();
    }

    /// Select all text
    fn select_all(&mut self, cx: &mut Context<Self>) {
        self.commit_ime_composition();
        if !self.text.is_empty() {
            self.selection = Some(0..self.text.len());
            self.set_cursor_canonical(self.text.len());
        } else {
            self.set_cursor_canonical(self.cursor);
        }
        cx.notify();
    }

    /// Select the word (or whitespace run) at a byte offset — the native
    /// double-click contract.
    fn select_word_at(&mut self, offset: usize, cx: &mut Context<Self>) {
        let range = self.word_run_at(offset);
        if !range.is_empty() {
            self.set_cursor_canonical(range.end);
            self.selection = Some(range);
        }
        cx.notify();
    }

    /// Byte range of the contiguous word — or whitespace run — containing
    /// `offset`. A click at the very end of the text classifies by the
    /// preceding character.
    fn word_run_at(&self, offset: usize) -> std::ops::Range<usize> {
        let text = &self.text;
        let mut offset = offset.min(text.len());
        while offset > 0 && !text.is_char_boundary(offset) {
            offset -= 1;
        }
        let anchor = text[offset..]
            .chars()
            .next()
            .or_else(|| text[..offset].chars().next_back());
        let Some(anchor) = anchor else {
            return offset..offset;
        };
        let in_space = anchor.is_whitespace();
        let start = text[..offset]
            .char_indices()
            .rev()
            .take_while(|(_, c)| c.is_whitespace() == in_space)
            .last()
            .map(|(index, _)| index)
            .unwrap_or(offset);
        let end = text[offset..]
            .char_indices()
            .take_while(|(_, c)| c.is_whitespace() == in_space)
            .last()
            .map(|(index, c)| offset + index + c.len_utf8())
            .unwrap_or(offset);
        start..end
    }

    /// Extend selection to new position
    fn extend_selection(&mut self, new_pos: usize) {
        let (anchor, _) = match &self.selection {
            Some(range) => {
                if self.cursor == range.start {
                    (range.end, range.start)
                } else {
                    (range.start, range.end)
                }
            }
            None => (self.cursor, self.cursor),
        };

        self.set_cursor_canonical(new_pos);
        if anchor != new_pos {
            self.selection = Some(anchor.min(new_pos)..anchor.max(new_pos));
        } else {
            self.selection = None;
        }
    }

    /// Copy selected text to clipboard
    fn copy(&self, _window: &mut Window, cx: &mut Context<Self>) {
        if let Some(ref range) = self.selection {
            let text = &self.text[range.clone()];
            if !text.is_empty() {
                cx.write_to_clipboard(ClipboardItem::new_string(text.to_string()));
            }
        }
    }

    /// Cut selected text to clipboard
    fn cut(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(range) = self.selection.clone().filter(|range| !range.is_empty()) else {
            return;
        };
        let text = &self.text[range];
        cx.write_to_clipboard(ClipboardItem::new_string(text.to_string()));
        self.backspace(window, cx);
    }

    /// Paste from clipboard
    fn paste(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = crate::utils::read_text(cx) {
            self.insert(&text, window, cx);
        }
    }

    /// Undo last change
    fn undo(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        if self.cancel_ime_composition() {
            cx.emit(InputEvent::Change);
            cx.notify();
            return;
        }

        if let Some(entry) = self.undo_stack.pop() {
            self.redo_stack.push(self.history_entry());

            self.text = entry.text;
            self.cursor = entry.cursor;
            self.caret_affinity = entry.caret_affinity;
            self.selection = entry.selection;
            self.ime_marked_range = None;
            self.ime_composition_start = None;

            cx.emit(InputEvent::Change);
            cx.notify();
        }
    }

    /// Redo last undone change
    fn redo(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        if self.cancel_ime_composition() {
            cx.emit(InputEvent::Change);
            cx.notify();
            return;
        }

        if let Some(entry) = self.redo_stack.pop() {
            let current = self.history_entry();
            self.push_undo_entry(current);

            self.text = entry.text;
            self.cursor = entry.cursor;
            self.caret_affinity = entry.caret_affinity;
            self.selection = entry.selection;
            self.ime_marked_range = None;
            self.ime_composition_start = None;

            cx.emit(InputEvent::Change);
            cx.notify();
        }
    }

    /// Save current state for undo
    fn save_undo_state(&mut self) {
        let entry = self.history_entry();
        self.push_undo_entry(entry);
    }

    fn history_entry(&self) -> HistoryEntry {
        HistoryEntry {
            text: self.text.clone(),
            cursor: self.cursor,
            caret_affinity: self.caret_affinity,
            selection: self.selection.clone(),
        }
    }

    fn push_undo_entry(&mut self, entry: HistoryEntry) {
        self.undo_stack.push(entry);
        if self.undo_stack.len() > 100 {
            self.undo_stack.remove(0);
        }
    }

    fn begin_ime_composition(&mut self) {
        if self.ime_composition_start.is_none() {
            self.ime_composition_start = Some(self.history_entry());
            self.redo_stack.clear();
        }
    }

    fn composition_changed_from(&self, entry: &HistoryEntry) -> bool {
        self.text != entry.text
            || self.cursor != entry.cursor
            || self.caret_affinity != entry.caret_affinity
            || self.selection != entry.selection
    }

    /// Commits the current marked text as one undoable edit.
    fn commit_ime_composition(&mut self) -> bool {
        self.ime_marked_range = None;
        let Some(entry) = self.ime_composition_start.take() else {
            return false;
        };
        let changed = self.composition_changed_from(&entry);
        if changed {
            self.push_undo_entry(entry);
        }
        changed
    }

    /// Drops transient marked text and restores the exact pre-composition
    /// selection, so a canceled replacement does not erase it.
    fn cancel_ime_composition(&mut self) -> bool {
        self.ime_marked_range = None;
        let Some(entry) = self.ime_composition_start.take() else {
            return false;
        };
        let changed = self.composition_changed_from(&entry);
        self.text = entry.text;
        self.cursor = entry.cursor;
        self.caret_affinity = entry.caret_affinity;
        self.selection = entry.selection;
        changed
    }

    fn prev_grapheme_boundary(&self, offset: usize) -> usize {
        let slice = &self.text[..offset];
        slice
            .grapheme_indices(true)
            .next_back()
            .map(|(i, _)| i)
            .unwrap_or(0)
    }

    fn next_grapheme_boundary(&self, offset: usize) -> usize {
        let slice = &self.text[offset..];
        slice
            .grapheme_indices(true)
            .nth(1)
            .map(|(i, _)| offset + i)
            .unwrap_or(self.text.len())
    }

    fn prev_word_boundary(&self, offset: usize) -> usize {
        let slice = &self.text[..offset];
        let mut pos = offset;
        for (i, c) in slice.char_indices().rev() {
            if !c.is_whitespace() {
                pos = i;
                break;
            }
            pos = i;
        }
        let slice = &self.text[..pos];
        for (i, c) in slice.char_indices().rev() {
            if c.is_whitespace() {
                return i + c.len_utf8();
            }
        }
        0
    }

    fn next_word_boundary(&self, offset: usize) -> usize {
        let slice = &self.text[offset..];
        let mut in_word = false;
        for (i, c) in slice.char_indices() {
            if c.is_whitespace() {
                if in_word {
                    return offset + i;
                }
            } else {
                in_word = true;
            }
        }
        self.text.len()
    }

    fn on_focus(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        cx.emit(InputEvent::Focus);
        cx.notify();
    }

    fn on_blur(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        self.commit_ime_composition();
        cx.emit(InputEvent::Blur);
        cx.notify();
    }

    /// Replace the placeholder shown while the input is empty.
    pub fn set_placeholder(&mut self, placeholder: &str, cx: &mut Context<Self>) {
        if self.placeholder.as_ref() != placeholder {
            self.placeholder = SharedString::from(placeholder.to_string());
            cx.notify();
        }
    }

    pub fn placeholder(&self) -> &str {
        &self.placeholder
    }

    pub fn is_disabled(&self) -> bool {
        self.disabled
    }

    pub fn focus_handle(&self) -> &FocusHandle {
        &self.focus_handle
    }

    pub fn spell_check_ranges(&self) -> &[Range<usize>] {
        &self.spell_check_ranges
    }
}

impl Focusable for InputState {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl EntityInputHandler for InputState {
    fn text_for_range(
        &mut self,
        range: Range<usize>,
        adjusted_range: &mut Option<Range<usize>>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<String> {
        let utf8_range = self.utf16_to_utf8_range(range);
        if utf8_range.start <= self.text.len() && utf8_range.end <= self.text.len() {
            adjusted_range.replace(self.utf8_range_to_utf16(utf8_range.clone()));
            Some(self.text[utf8_range].to_string())
        } else {
            None
        }
    }

    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        let (start, end) = match &self.selection {
            Some(range) => (range.start, range.end),
            None => (self.cursor, self.cursor),
        };

        let start_utf16 = self.utf8_to_utf16_offset(start);
        let end_utf16 = self.utf8_to_utf16_offset(end);
        let reversed = self
            .selection
            .as_ref()
            .is_some_and(|range| self.cursor == range.start);

        Some(UTF16Selection {
            range: start_utf16..end_utf16,
            reversed,
        })
    }

    fn marked_text_range(
        &self,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Range<usize>> {
        self.valid_marked_range().map(|range| {
            let start = self.utf8_to_utf16_offset(range.start);
            let end = self.utf8_to_utf16_offset(range.end);
            start..end
        })
    }

    fn unmark_text(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        self.commit_ime_composition();
        cx.notify();
    }

    fn replace_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let text = normalize_single_line_text(text);
        let utf8_range = range.map(|range| self.utf16_to_utf8_range(range));
        let marked_range = self.valid_marked_range();

        if self.ime_composition_start.is_some() {
            if text.is_empty() && (utf8_range.is_none() || utf8_range == marked_range) {
                if self.cancel_ime_composition() {
                    cx.emit(InputEvent::Change);
                }
                cx.notify();
                return;
            }

            let range = utf8_range
                .or(marked_range)
                .unwrap_or(self.cursor..self.cursor);
            self.text.replace_range(range.clone(), &text);
            self.set_cursor_canonical(range.start + text.len());
            self.selection = None;
            self.commit_ime_composition();
            cx.emit(InputEvent::Change);
            cx.notify();
            return;
        }

        self.commit_ime_composition();

        if let Some(range) = utf8_range {
            self.save_undo_state();
            self.text.replace_range(range.clone(), &text);
            self.set_cursor_canonical(range.start + text.len());
            self.selection = None;
        } else if let Some(sel_range) = self.selection.take() {
            self.save_undo_state();
            self.text.replace_range(sel_range.clone(), &text);
            self.set_cursor_canonical(sel_range.start + text.len());
        } else {
            self.save_undo_state();
            self.text.insert_str(self.cursor, &text);
            self.set_cursor_canonical(self.cursor + text.len());
        }

        self.redo_stack.clear();
        cx.emit(InputEvent::Change);
        cx.notify();
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        new_text: &str,
        new_selected_range: Option<Range<usize>>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let new_text = normalize_single_line_text(new_text);
        if new_text.is_empty() {
            if self.cancel_ime_composition() {
                cx.emit(InputEvent::Change);
            }
            cx.notify();
            return;
        }

        self.begin_ime_composition();
        let utf8_range = range.map(|r| self.utf16_to_utf8_range(r));

        let insert_start = if let Some(range) = utf8_range {
            self.text.replace_range(range.clone(), &new_text);
            range.start
        } else if let Some(range) = self.valid_marked_range() {
            self.text.replace_range(range.clone(), &new_text);
            range.start
        } else if let Some(sel_range) = self.selection.take() {
            self.text.replace_range(sel_range.clone(), &new_text);
            sel_range.start
        } else {
            self.text.insert_str(self.cursor, &new_text);
            self.cursor
        };

        self.ime_marked_range = Some(insert_start..insert_start + new_text.len());

        if let Some(selected) = new_selected_range {
            let utf8_selected = Self::utf16_range_to_utf8(&new_text, selected);
            self.set_cursor_canonical(insert_start + utf8_selected.start);
        } else {
            self.set_cursor_canonical(insert_start + new_text.len());
        }

        self.selection = None;
        cx.emit(InputEvent::Change);
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        element_bounds: Bounds<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let (Some(text_run), Some(font_size), Some(line_height)) = (
            self.pointer_text_run.as_ref(),
            self.pointer_font_size,
            self.pointer_line_height,
        ) else {
            return Some(element_bounds);
        };
        let layout = if self.pointer_text.as_deref() == Some(self.text.as_str()) {
            self.pointer_layout.clone()?
        } else {
            let mut text_run = text_run.clone();
            text_run.len = self.text.len();
            _window
                .text_system()
                .shape_line(self.text.clone().into(), font_size, &[text_run], None)
        };
        if layout.has_bidi_runs && !layout.bidi_geometry_complete {
            return Some(element_bounds);
        }

        let range = self.utf16_to_utf8_range(range_utf16.clone());
        let selection_range = self
            .selection
            .as_ref()
            .map(|selection| self.utf8_range_to_utf16(selection.clone()));
        let (index, affinity) = if selection_range.as_ref() == Some(&range_utf16) {
            (self.cursor.min(layout.len()), self.caret_affinity)
        } else {
            let index = range.end.min(layout.len());
            let affinity = if index == self.cursor {
                self.caret_affinity
            } else {
                self.canonical_affinity_for_index(index)
            };
            (index, affinity)
        };
        let Some(x) = layout.x_for_index_with_affinity_checked(index, affinity) else {
            return Some(element_bounds);
        };

        Some(Bounds {
            origin: Point::new(
                element_bounds.origin.x + x - self.scroll_x,
                element_bounds.origin.y
                    + (element_bounds.size.height - line_height) / 2.0
                    + px(2.0),
            ),
            size: size(px(2.0), line_height - px(4.0)),
        })
    }

    fn character_index_for_point(
        &mut self,
        _point: Point<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<usize> {
        Some(self.cursor)
    }
}

impl InputState {
    /// The IME composition range, only while it still addresses this text.
    fn valid_marked_range(&self) -> Option<Range<usize>> {
        self.ime_marked_range.clone().filter(|range| {
            range.start <= range.end
                && range.end <= self.text.len()
                && self.text.is_char_boundary(range.start)
                && self.text.is_char_boundary(range.end)
        })
    }

    fn utf8_to_utf16_offset(&self, utf8_offset: usize) -> usize {
        self.text[..self.text.floor_char_boundary(utf8_offset)]
            .encode_utf16()
            .count()
    }

    fn utf16_to_utf8_range(&self, utf16_range: Range<usize>) -> Range<usize> {
        Self::utf16_range_to_utf8(&self.text, utf16_range)
    }

    fn utf8_range_to_utf16(&self, utf8_range: Range<usize>) -> Range<usize> {
        self.utf8_to_utf16_offset(utf8_range.start)..self.utf8_to_utf16_offset(utf8_range.end)
    }

    fn utf16_offset_to_utf8_floor(text: &str, offset: usize) -> usize {
        let mut utf8_offset = 0;
        let mut utf16_offset = 0;
        for character in text.chars() {
            let next_utf16_offset = utf16_offset + character.len_utf16();
            if offset <= utf16_offset || offset < next_utf16_offset {
                return utf8_offset;
            }
            utf16_offset = next_utf16_offset;
            utf8_offset += character.len_utf8();
        }
        utf8_offset
    }

    fn utf16_offset_to_utf8_ceil(text: &str, offset: usize) -> usize {
        let mut utf8_offset = 0;
        let mut utf16_offset = 0;
        for character in text.chars() {
            if offset <= utf16_offset {
                return utf8_offset;
            }
            let next_utf16_offset = utf16_offset + character.len_utf16();
            let next_utf8_offset = utf8_offset + character.len_utf8();
            if offset <= next_utf16_offset {
                return next_utf8_offset;
            }
            utf16_offset = next_utf16_offset;
            utf8_offset = next_utf8_offset;
        }
        utf8_offset
    }

    fn utf16_range_to_utf8(text: &str, utf16_range: Range<usize>) -> Range<usize> {
        let start = utf16_range.start.min(utf16_range.end);
        let end = utf16_range.start.max(utf16_range.end);
        if start == end {
            let offset = Self::utf16_offset_to_utf8_floor(text, start);
            return offset..offset;
        }

        Self::utf16_offset_to_utf8_floor(text, start)..Self::utf16_offset_to_utf8_ceil(text, end)
    }
}

impl InputState {
    pub fn handle_backspace(&mut self, _: &Backspace, window: &mut Window, cx: &mut Context<Self>) {
        self.backspace(window, cx);
    }

    pub fn handle_delete(&mut self, _: &Delete, window: &mut Window, cx: &mut Context<Self>) {
        self.delete(window, cx);
    }

    pub fn handle_delete_word(
        &mut self,
        _: &DeleteWord,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.delete_word(window, cx);
    }

    pub fn handle_delete_word_forward(
        &mut self,
        _: &DeleteWordForward,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.delete_word_forward(window, cx);
    }

    pub fn handle_enter(&mut self, action: &Enter, _window: &mut Window, cx: &mut Context<Self>) {
        let _ = action.secondary;
        if self.valid_marked_range().is_some() {
            self.commit_ime_composition();
            cx.notify();
            return;
        }
        cx.emit(InputEvent::PressEnter);
    }

    pub fn handle_escape(&mut self, _: &Escape, _window: &mut Window, cx: &mut Context<Self>) {
        if self.cancel_ime_composition() {
            cx.emit(InputEvent::Change);
            cx.notify();
            return;
        }
        self.selection = None;
        self.set_cursor_canonical(self.cursor);
        cx.notify();
    }

    pub fn handle_move_left(&mut self, _: &MoveLeft, _window: &mut Window, cx: &mut Context<Self>) {
        self.move_left(cx);
    }

    pub fn handle_move_right(
        &mut self,
        _: &MoveRight,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.move_right(cx);
    }

    pub fn handle_move_to_start(
        &mut self,
        _: &MoveToStart,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.move_to_start(cx);
    }

    pub fn handle_move_to_end(
        &mut self,
        _: &MoveToEnd,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.move_to_end(cx);
    }

    pub fn handle_move_word_left(
        &mut self,
        _: &MoveWordLeft,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.move_word_left(cx);
    }

    pub fn handle_move_word_right(
        &mut self,
        _: &MoveWordRight,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.move_word_right(cx);
    }

    pub fn handle_select_left(
        &mut self,
        _: &SelectLeft,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.select_left(cx);
    }

    pub fn handle_select_right(
        &mut self,
        _: &SelectRight,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.select_right(cx);
    }

    pub fn handle_select_to_start(
        &mut self,
        _: &SelectToStart,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.select_to_start(cx);
    }

    pub fn handle_select_to_end(
        &mut self,
        _: &SelectToEnd,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.select_to_end(cx);
    }

    pub fn handle_select_word_left(
        &mut self,
        _: &SelectWordLeft,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.select_word_left(cx);
    }

    pub fn handle_select_word_right(
        &mut self,
        _: &SelectWordRight,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.select_word_right(cx);
    }

    pub fn handle_select_all(
        &mut self,
        _: &SelectAll,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.select_all(cx);
    }

    pub fn handle_copy(&mut self, _: &Copy, window: &mut Window, cx: &mut Context<Self>) {
        self.copy(window, cx);
    }

    pub fn handle_cut(&mut self, _: &Cut, window: &mut Window, cx: &mut Context<Self>) {
        self.cut(window, cx);
    }

    pub fn handle_paste(&mut self, _: &Paste, window: &mut Window, cx: &mut Context<Self>) {
        self.paste(window, cx);
    }

    pub fn handle_undo(&mut self, _: &Undo, window: &mut Window, cx: &mut Context<Self>) {
        self.undo(window, cx);
    }

    pub fn handle_redo(&mut self, _: &Redo, window: &mut Window, cx: &mut Context<Self>) {
        self.redo(window, cx);
    }
}

impl Render for InputState {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
    }
}

#[cfg(test)]
mod tests {
    use super::{InputState, normalize_single_line_text};
    use core::prelude::v1::test;

    #[test]
    fn normalizes_all_supported_line_separators_without_joining_words() {
        assert_eq!(
            normalize_single_line_text(
                "first\r\nsecond\nthird\rfourth\u{000B}fifth\u{000C}sixth\u{0085}seventh\u{2028}eighth\u{2029}ninth"
            ),
            "first second third fourth fifth sixth seventh eighth ninth"
        );
    }

    #[test]
    fn leaves_single_line_unicode_text_unchanged() {
        assert_eq!(
            normalize_single_line_text("chat → こんにちは"),
            "chat → こんにちは"
        );
    }

    #[test]
    fn utf16_ranges_preserve_astral_character_boundaries() {
        let text = "a😀b";

        assert_eq!(InputState::utf16_range_to_utf8(text, 1..1), 1..1);
        assert_eq!(InputState::utf16_range_to_utf8(text, 1..2), 1..5);
        assert_eq!(InputState::utf16_range_to_utf8(text, 2..3), 1..5);
        assert_eq!(InputState::utf16_range_to_utf8(text, 3..3), 5..5);
        assert_eq!(InputState::utf16_range_to_utf8(text, 2..1), 1..5);
    }
}
