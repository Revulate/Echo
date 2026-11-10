//! Input Manager - Extracted from app.rs monolith
//!
//! Handles:
//! - Per-split input text management
//! - Message history navigation (up/down arrows)
//! - Placeholder input management
//!
//! Note: Reply state is now per-split in SplitPane.
//! Note: Focus management remains in EchoApp since FocusHandle requires App context.

use crate::components::ui::input::InputState;
use crate::state::SplitId;
use echo_core::followed_live::FollowedLiveStream;
use gpui::{Entity, ScrollHandle, Subscription};
use std::collections::HashMap;
use std::ops::Range;
use std::rc::Rc;
use std::time::{Duration, Instant};
use unicode_segmentation::UnicodeSegmentation;

/// Keep per-split draft editing history bounded just like the shared Input
/// component. The composer is intentionally custom-rendered, but it should
/// never lose normal text-editor affordances as a result.
const MAX_DRAFT_UNDO_HISTORY: usize = 100;
const MAX_DRAFT_UNDO_BYTES: usize = 1024 * 1024;
const DRAFT_EDIT_PAUSE: Duration = Duration::from_secs(1);

#[derive(Clone, Copy, PartialEq, Eq)]
enum DraftEditKind {
    Typing,
    Backspace,
    Delete,
    WordBackspace,
    WordDelete,
}

struct DraftEditTransaction {
    kind: DraftEditKind,
    next_cursor: usize,
    next_len: usize,
    edited_at: Instant,
}

#[derive(Clone, Debug)]
struct DraftEditSnapshot {
    text: String,
    cursor: usize,
    selection: Option<Range<usize>>,
}

/// One keyboard-selectable row in an empty split's channel chooser.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ChannelSuggestion {
    /// A recent or already open channel.
    Recent(String),
    /// A followed channel that is live, listed under Live now.
    Live(String),
}

impl ChannelSuggestion {
    pub fn login(&self) -> &str {
        match self {
            Self::Recent(login) | Self::Live(login) => login,
        }
    }
}

/// A chooser's Live now section, kept until the followed-live version, the
/// query or streamer mode changes, so re-renders such as a caret blink do not
/// re-match the whole follow list.
#[derive(Clone, Default)]
pub struct ChooserLiveRows {
    /// `(followed_live::version, query, streamer mode)` the rows were built for.
    pub key: Option<(u64, String, bool)>,
    pub rows: Vec<FollowedLiveStream>,
    /// Matches left out by the row cap.
    pub more: usize,
    /// Line shown when there are no rows.
    pub notice: Option<&'static str>,
}

#[derive(Clone)]
pub struct PlaceholderInput {
    pub input: Entity<InputState>,
    pub query: String,
    /// Recent rows first, then Live now rows, in display order.
    pub suggestions: Vec<ChannelSuggestion>,
    pub selected: Option<usize>,
    pub scroll: ScrollHandle,
    pub live: ChooserLiveRows,
    /// Tracks the Live now group's children inside the scrolled list.
    pub live_scroll: ScrollHandle,
    pub _subscriptions: Rc<Vec<Subscription>>,
}

/// Manager entity for input state and behavior
///
/// Note: Focus handle is kept in EchoApp since FocusHandle requires App context.
/// Note: Reply state is per-split in SplitPane, not here.
/// This struct manages input data/state only.
pub struct InputManager {
    pub input_texts: HashMap<SplitId, String>,
    pub cursor_positions: HashMap<SplitId, usize>,
    selection_ranges: HashMap<SplitId, Range<usize>>,
    focused_split: Option<SplitId>,
    edit_transactions: HashMap<SplitId, DraftEditTransaction>,
    undo_stacks: HashMap<SplitId, Vec<DraftEditSnapshot>>,
    redo_stacks: HashMap<SplitId, Vec<DraftEditSnapshot>>,
    ime_composition_starts: HashMap<SplitId, DraftEditSnapshot>,

    pub input_history: HashMap<String, Vec<String>>,
    pub history_index: HashMap<SplitId, Option<usize>>,
    pub temp_input: HashMap<SplitId, Option<String>>,
    recalled_text: HashMap<SplitId, String>,

    pub placeholder_inputs: HashMap<SplitId, PlaceholderInput>,
    pub focused_placeholder: Option<SplitId>,
}

impl InputManager {
    pub fn new() -> Self {
        Self {
            input_texts: HashMap::new(),
            cursor_positions: HashMap::new(),
            selection_ranges: HashMap::new(),
            focused_split: None,
            edit_transactions: HashMap::new(),
            undo_stacks: HashMap::new(),
            redo_stacks: HashMap::new(),
            ime_composition_starts: HashMap::new(),
            input_history: HashMap::new(),
            history_index: HashMap::new(),
            temp_input: HashMap::new(),
            recalled_text: HashMap::new(),
            placeholder_inputs: HashMap::new(),
            focused_placeholder: None,
        }
    }
}

impl Default for InputManager {
    fn default() -> Self {
        Self::new()
    }
}

impl InputManager {
    /// Current draft text for a split, or `""` when no draft exists.
    pub fn text_for_split(&self, split_id: SplitId) -> &str {
        self.input_texts
            .get(&split_id)
            .map(String::as_str)
            .unwrap_or("")
    }

    pub fn cursor_for_split(&self, split_id: SplitId) -> usize {
        let text = self
            .input_texts
            .get(&split_id)
            .map(String::as_str)
            .unwrap_or("");
        let cursor = self
            .cursor_positions
            .get(&split_id)
            .copied()
            .unwrap_or(text.len());
        Self::clamp_cursor(text, cursor)
    }

    /// Current non-empty UTF-8-safe selection for a composer draft.
    pub fn selection_for_split(&self, split_id: SplitId) -> Option<Range<usize>> {
        let range = self.selection_ranges.get(&split_id)?.clone();
        let text = self
            .input_texts
            .get(&split_id)
            .map(String::as_str)
            .unwrap_or("");
        (range.start < range.end
            && range.end <= text.len()
            && text.is_char_boundary(range.start)
            && text.is_char_boundary(range.end))
        .then_some(range)
    }

    pub fn set_cursor_for_split(&mut self, split_id: SplitId, cursor: usize) {
        self.break_edit_transaction(split_id);
        let text = self
            .input_texts
            .get(&split_id)
            .map(String::as_str)
            .unwrap_or("");
        self.cursor_positions
            .insert(split_id, Self::clamp_cursor(text, cursor));
        self.selection_ranges.remove(&split_id);
    }

    /// Restore an exact cursor/selection snapshot owned by the disposable UI
    /// harness without recording undo history. Ordinary editing paths should
    /// use the cursor/selection operations above.
    pub(crate) fn restore_pointer_state(
        &mut self,
        split_id: SplitId,
        cursor: Option<usize>,
        selection: Option<Range<usize>>,
    ) {
        self.break_edit_transaction(split_id);
        let text = self
            .input_texts
            .get(&split_id)
            .map(String::as_str)
            .unwrap_or("");
        match cursor {
            Some(cursor) => {
                self.cursor_positions
                    .insert(split_id, Self::clamp_cursor(text, cursor));
            }
            None => {
                self.cursor_positions.remove(&split_id);
            }
        }
        if let Some(selection) = selection.filter(|range| {
            range.start < range.end
                && range.end <= text.len()
                && text.is_char_boundary(range.start)
                && text.is_char_boundary(range.end)
        }) {
            self.selection_ranges.insert(split_id, selection);
        } else {
            self.selection_ranges.remove(&split_id);
        }
    }

    /// Move the caret and update a mouse-drag selection without recording an
    /// undo entry. Both offsets are clamped to UTF-8 character boundaries; the
    /// caret remains at the moving focus edge while the stable anchor can sit
    /// on either side, matching native reversed-selection behavior.
    pub fn set_pointer_selection(
        &mut self,
        split_id: SplitId,
        anchor: usize,
        focus: usize,
    ) -> bool {
        self.break_edit_transaction(split_id);
        let text = self
            .input_texts
            .get(&split_id)
            .map(String::as_str)
            .unwrap_or("");
        let anchor = Self::clamp_cursor(text, anchor);
        let focus = Self::clamp_cursor(text, focus);
        let previous_cursor = self.cursor_for_split(split_id);
        let previous_selection = self.selection_for_split(split_id);
        let next_selection = (anchor != focus).then_some(anchor.min(focus)..anchor.max(focus));

        self.cursor_positions.insert(split_id, focus);
        if let Some(selection) = next_selection.clone() {
            self.selection_ranges.insert(split_id, selection);
        } else {
            self.selection_ranges.remove(&split_id);
        }

        previous_cursor != focus || previous_selection != next_selection
    }

    /// Insert text at the caret, replacing an active selection when present.
    /// Returns whether the draft changed.
    pub fn insert_text_at_cursor(&mut self, split_id: SplitId, inserted: &str) -> bool {
        if inserted.is_empty() {
            return false;
        }

        self.record_undo_state(split_id);
        let cursor = self.cursor_for_split(split_id);
        let selection = self.selection_for_split(split_id);
        let text = self.input_texts.entry(split_id).or_default();
        let next_cursor = if let Some(range) = selection {
            text.replace_range(range.clone(), inserted);
            range.start + inserted.len()
        } else {
            text.insert_str(cursor, inserted);
            cursor + inserted.len()
        };
        self.cursor_positions.insert(split_id, next_cursor);
        self.selection_ranges.remove(&split_id);
        true
    }

    /// Replace an explicit UTF-8 range, or the current selection/caret when no
    /// range is supplied. The caller controls whether this starts a new undo
    /// step so repeated IME composition updates remain one logical edit.
    pub fn replace_text_range(
        &mut self,
        split_id: SplitId,
        range: Option<Range<usize>>,
        inserted: &str,
        record_undo: bool,
    ) -> Option<Range<usize>> {
        self.set_focused_split(Some(split_id));
        let text = self
            .input_texts
            .get(&split_id)
            .map(String::as_str)
            .unwrap_or("");
        let typing = range.is_none()
            && self.selection_for_split(split_id).is_none()
            && !inserted.is_empty()
            && inserted.graphemes(true).count() == 1;
        let range = range
            .or_else(|| self.selection_for_split(split_id))
            .unwrap_or_else(|| {
                let cursor = self.cursor_for_split(split_id);
                cursor..cursor
            });
        if range.start > range.end
            || range.end > text.len()
            || !text.is_char_boundary(range.start)
            || !text.is_char_boundary(range.end)
        {
            return None;
        }
        if record_undo {
            if typing {
                self.record_grouped_edit(split_id, DraftEditKind::Typing);
            } else {
                self.record_undo_state(split_id);
            }
        }
        self.input_texts
            .entry(split_id)
            .or_default()
            .replace_range(range.clone(), inserted);
        let inserted_range = range.start..range.start + inserted.len();
        self.cursor_positions.insert(split_id, inserted_range.end);
        self.selection_ranges.remove(&split_id);
        if record_undo {
            self.redo_stacks.remove(&split_id);
        }
        if record_undo && typing {
            self.finish_grouped_edit(split_id, DraftEditKind::Typing);
        } else {
            self.break_edit_transaction(split_id);
        }
        Some(inserted_range)
    }

    /// Replace an exact draft fragment and ensure a trailing token delimiter.
    ///
    /// The edit is atomic: stale or invalid ranges leave both text and cursor
    /// untouched. Existing surrounding whitespace is preserved verbatim.
    pub fn replace_range_with_token(
        &mut self,
        split_id: SplitId,
        range: Range<usize>,
        expected_fragment: &str,
        token: &str,
    ) -> bool {
        if token.is_empty() {
            return false;
        }

        let current_text = self
            .input_texts
            .get(&split_id)
            .map(String::as_str)
            .unwrap_or("");
        if range.start > range.end
            || range.end > current_text.len()
            || !current_text.is_char_boundary(range.start)
            || !current_text.is_char_boundary(range.end)
            || &current_text[range.clone()] != expected_fragment
        {
            return false;
        }

        let following_text = &current_text[range.end..];
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

        let mut replacement = String::with_capacity(token.len() + usize::from(add_trailing_space));
        replacement.push_str(token);
        if add_trailing_space {
            replacement.push(' ');
        }

        let cursor = range.start + replacement.len() + existing_whitespace_len;
        self.record_undo_state(split_id);
        self.input_texts
            .entry(split_id)
            .or_default()
            .replace_range(range, &replacement);
        self.cursor_positions.insert(split_id, cursor);
        self.selection_ranges.remove(&split_id);
        true
    }

    /// Insert a token at the current cursor using the same boundary rules as
    /// autocomplete replacement. Direct picker insertion still separates
    /// itself from preceding non-whitespace; completion replacements never
    /// invent space before their captured range.
    pub fn insert_token_at_cursor(&mut self, split_id: SplitId, token: &str) -> bool {
        let range = self.selection_for_split(split_id).unwrap_or_else(|| {
            let cursor = self.cursor_for_split(split_id);
            cursor..cursor
        });
        let current_text = self.text_for_split(split_id);
        let expected_fragment = current_text[range.clone()].to_string();
        let needs_leading_space = self
            .text_for_split(split_id)
            .get(..range.start)
            .and_then(|before| before.chars().next_back())
            .is_some_and(|ch| !ch.is_whitespace());
        if needs_leading_space {
            self.replace_range_with_token(split_id, range, &expected_fragment, &format!(" {token}"))
        } else {
            self.replace_range_with_token(split_id, range, &expected_fragment, token)
        }
    }

    pub fn set_focused_split(&mut self, split_id: Option<SplitId>) {
        if self.focused_split != split_id {
            if let Some(previous) = self.focused_split {
                self.break_edit_transaction(previous);
            }
            if let Some(next) = split_id {
                self.break_edit_transaction(next);
            }
            self.focused_split = split_id;
        }
    }

    pub fn move_cursor_to(
        &mut self,
        split_id: SplitId,
        next: usize,
        extend_selection: bool,
    ) -> bool {
        self.break_edit_transaction(split_id);
        let cursor = self.cursor_for_split(split_id);
        let selection = self.selection_for_split(split_id);
        let next = Self::clamp_cursor(self.text_for_split(split_id), next);
        self.cursor_positions.insert(split_id, next);
        self.update_selection_from_anchor(
            split_id,
            selection.clone(),
            cursor,
            next,
            extend_selection,
        );
        next != cursor || self.selection_for_split(split_id) != selection
    }

    pub fn move_cursor_left(
        &mut self,
        split_id: SplitId,
        by_word: bool,
        extend_selection: bool,
    ) -> bool {
        self.break_edit_transaction(split_id);
        let cursor = self.cursor_for_split(split_id);
        let selection = self.selection_for_split(split_id);
        if !extend_selection && let Some(range) = selection {
            self.cursor_positions.insert(split_id, range.start);
            self.selection_ranges.remove(&split_id);
            return true;
        }
        let text = self
            .input_texts
            .get(&split_id)
            .map(String::as_str)
            .unwrap_or("");
        let next = if by_word {
            Self::previous_word_boundary(text, cursor)
        } else {
            Self::previous_grapheme_boundary(text, cursor)
        };
        if next != cursor {
            self.cursor_positions.insert(split_id, next);
            self.update_selection_from_anchor(split_id, selection, cursor, next, extend_selection);
            return true;
        }
        false
    }

    pub fn move_cursor_right(
        &mut self,
        split_id: SplitId,
        by_word: bool,
        extend_selection: bool,
    ) -> bool {
        self.break_edit_transaction(split_id);
        let cursor = self.cursor_for_split(split_id);
        let selection = self.selection_for_split(split_id);
        if !extend_selection && let Some(range) = selection {
            self.cursor_positions.insert(split_id, range.end);
            self.selection_ranges.remove(&split_id);
            return true;
        }
        let text = self
            .input_texts
            .get(&split_id)
            .map(String::as_str)
            .unwrap_or("");
        let next = if by_word {
            Self::next_word_boundary(text, cursor)
        } else {
            Self::next_grapheme_boundary(text, cursor)
        };
        if next != cursor {
            self.cursor_positions.insert(split_id, next);
            self.update_selection_from_anchor(split_id, selection, cursor, next, extend_selection);
            return true;
        }
        false
    }

    pub fn delete_backward(&mut self, split_id: SplitId, by_word: bool) -> bool {
        self.set_focused_split(Some(split_id));
        let cursor = self.cursor_for_split(split_id);
        let selection = self.selection_for_split(split_id);
        if selection.is_none() && cursor == 0 {
            return false;
        }
        let grouped = selection.is_none();
        let kind = if by_word {
            DraftEditKind::WordBackspace
        } else {
            DraftEditKind::Backspace
        };
        if grouped {
            self.record_grouped_edit(split_id, kind);
        } else {
            self.record_undo_state(split_id);
        }
        let text = self.input_texts.entry(split_id).or_default();
        let start = if let Some(range) = selection {
            text.replace_range(range.clone(), "");
            range.start
        } else {
            let start = if by_word {
                Self::previous_word_boundary(text, cursor)
            } else {
                Self::previous_grapheme_boundary(text, cursor)
            };
            text.replace_range(start..cursor, "");
            start
        };
        self.cursor_positions.insert(split_id, start);
        self.selection_ranges.remove(&split_id);
        if grouped {
            self.finish_grouped_edit(split_id, kind);
        }
        self.end_history_navigation(split_id);
        true
    }

    pub fn delete_forward(&mut self, split_id: SplitId, by_word: bool) -> bool {
        self.set_focused_split(Some(split_id));
        let cursor = self.cursor_for_split(split_id);
        let selection = self.selection_for_split(split_id);
        let text_len = self
            .input_texts
            .get(&split_id)
            .map(|text| text.len())
            .unwrap_or_default();
        if selection.is_none() && cursor >= text_len {
            return false;
        }
        let grouped = selection.is_none();
        let kind = if by_word {
            DraftEditKind::WordDelete
        } else {
            DraftEditKind::Delete
        };
        if grouped {
            self.record_grouped_edit(split_id, kind);
        } else {
            self.record_undo_state(split_id);
        }
        let text = self.input_texts.entry(split_id).or_default();
        let next_cursor = if let Some(range) = selection {
            text.replace_range(range.clone(), "");
            range.start
        } else {
            let end = if by_word {
                Self::next_word_boundary(text, cursor)
            } else {
                Self::next_grapheme_boundary(text, cursor)
            };
            text.replace_range(cursor..end, "");
            cursor
        };
        self.cursor_positions.insert(split_id, next_cursor);
        self.selection_ranges.remove(&split_id);
        if grouped {
            self.finish_grouped_edit(split_id, kind);
        }
        self.end_history_navigation(split_id);
        true
    }

    /// Select the entire draft, matching Ctrl/Cmd+A behavior in a native input.
    /// Select the word (or whitespace run) containing `offset` — the native
    /// double-click contract. Returns false when nothing changes.
    pub fn select_word_at(&mut self, split_id: SplitId, offset: usize) -> bool {
        self.break_edit_transaction(split_id);
        let Some(text) = self.input_texts.get(&split_id) else {
            return false;
        };
        let range = Self::word_run_at(text, offset);
        if range.is_empty() {
            return false;
        }
        let previous = self.selection_for_split(split_id);
        if previous.as_ref() == Some(&range) && self.cursor_for_split(split_id) == range.end {
            return false;
        }
        self.cursor_positions.insert(split_id, range.end);
        self.selection_ranges.insert(split_id, range);
        true
    }

    /// Byte range of the contiguous word — or whitespace run — containing
    /// `offset`. A click at the very end of the text classifies by the
    /// preceding character.
    fn word_run_at(text: &str, offset: usize) -> std::ops::Range<usize> {
        let offset = Self::clamp_cursor(text, offset);
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

    pub fn select_all(&mut self, split_id: SplitId) -> bool {
        self.break_edit_transaction(split_id);
        let text_len = self
            .input_texts
            .get(&split_id)
            .map(|text| text.len())
            .unwrap_or_default();
        if text_len == 0 {
            return false;
        }
        let previous = self.selection_for_split(split_id);
        if previous.as_ref() == Some(&(0..text_len)) && self.cursor_for_split(split_id) == text_len
        {
            return false;
        }
        self.selection_ranges.insert(split_id, 0..text_len);
        self.cursor_positions.insert(split_id, text_len);
        true
    }

    /// Copy the active selection without changing the draft.
    pub fn copy_selection(&self, split_id: SplitId) -> Option<String> {
        let range = self.selection_for_split(split_id)?;
        self.input_texts
            .get(&split_id)
            .map(|text| text[range].to_string())
            .filter(|text| !text.is_empty())
    }

    /// Cut the active selection and return the copied text.
    pub fn cut_selection(&mut self, split_id: SplitId) -> Option<String> {
        let range = self.selection_for_split(split_id)?;
        let copied = self
            .input_texts
            .get(&split_id)
            .map(|text| text[range.clone()].to_string())?;
        if copied.is_empty() {
            return None;
        }
        self.record_undo_state(split_id);
        if let Some(text) = self.input_texts.get_mut(&split_id) {
            text.replace_range(range.clone(), "");
        }
        self.cursor_positions.insert(split_id, range.start);
        self.selection_ranges.remove(&split_id);
        Some(copied)
    }

    /// Delete only the exact selection snapshot a deferred menu action was
    /// opened for. This prevents Cut from copying one range and later deleting
    /// a different draft range after state changes.
    pub fn delete_exact_selection(
        &mut self,
        split_id: SplitId,
        range: Range<usize>,
        expected_text: &str,
    ) -> Option<String> {
        if self.selection_for_split(split_id).as_ref() != Some(&range) {
            return None;
        }
        let text = self.input_texts.get(&split_id)?;
        if text.get(range.clone()) != Some(expected_text) {
            return None;
        }
        let removed = expected_text.to_string();
        self.record_undo_state(split_id);
        self.input_texts
            .get_mut(&split_id)?
            .replace_range(range.clone(), "");
        self.cursor_positions.insert(split_id, range.start);
        self.selection_ranges.remove(&split_id);
        Some(removed)
    }

    pub fn can_undo(&self, split_id: SplitId) -> bool {
        self.undo_stacks
            .get(&split_id)
            .is_some_and(|stack| !stack.is_empty())
    }

    pub fn can_redo(&self, split_id: SplitId) -> bool {
        self.redo_stacks
            .get(&split_id)
            .is_some_and(|stack| !stack.is_empty())
    }

    /// Restore the previous split-local draft edit, if any.
    pub fn undo(&mut self, split_id: SplitId) -> bool {
        if self.cancel_ime_composition(split_id) {
            return true;
        }
        self.break_edit_transaction(split_id);
        let Some(snapshot) = self
            .undo_stacks
            .get_mut(&split_id)
            .and_then(|stack| stack.pop())
        else {
            return false;
        };
        let current = self.snapshot(split_id);
        Self::push_history_snapshot(self.redo_stacks.entry(split_id).or_default(), current);
        self.restore_snapshot(split_id, snapshot);
        true
    }

    /// Reapply a draft edit that was previously undone, if any.
    pub fn redo(&mut self, split_id: SplitId) -> bool {
        if self.cancel_ime_composition(split_id) {
            return true;
        }
        self.break_edit_transaction(split_id);
        let Some(snapshot) = self
            .redo_stacks
            .get_mut(&split_id)
            .and_then(|stack| stack.pop())
        else {
            return false;
        };
        let current = self.snapshot(split_id);
        Self::push_history_snapshot(self.undo_stacks.entry(split_id).or_default(), current);
        self.restore_snapshot(split_id, snapshot);
        true
    }

    /// Clear only the editable-draft state for a split. Channel history and
    /// placeholder-channel state intentionally remain owned by their existing
    /// lifecycle paths.
    pub fn clear_draft_for_split(&mut self, split_id: SplitId) {
        self.break_edit_transaction(split_id);
        self.ime_composition_starts.remove(&split_id);
        self.input_texts.remove(&split_id);
        self.cursor_positions.remove(&split_id);
        self.selection_ranges.remove(&split_id);
        self.undo_stacks.remove(&split_id);
        self.redo_stacks.remove(&split_id);
        self.reset_history_navigation(split_id);
    }

    /// Forget the history position and the stashed draft, e.g. when the
    /// split's channel changes.
    pub fn reset_history_navigation(&mut self, split_id: SplitId) {
        self.break_edit_transaction(split_id);
        self.history_index.remove(&split_id);
        self.temp_input.remove(&split_id);
        self.recalled_text.remove(&split_id);
    }

    /// Insert an emote into the input at the given channel/path
    /// Adds proper spacing around the emote name
    pub fn insert_emote(&mut self, split_id: SplitId, emote_name: &str) {
        let _ = self.insert_token_at_cursor(split_id, emote_name);
    }

    /// Insert an @mention into the input at the given channel/path
    /// Adds @ prefix and proper spacing
    pub fn insert_mention(&mut self, split_id: SplitId, username: &str) {
        let mention = format!("@{}", username);
        let _ = self.insert_token_at_cursor(split_id, &mention);
    }

    /// Prepare a whisper command without overwriting an unrelated active draft.
    pub fn compose_whisper(&mut self, split_id: SplitId, username: &str) -> bool {
        let trimmed = self
            .input_texts
            .get(&split_id)
            .map(|input_text| input_text.trim_start().to_string())
            .unwrap_or_default();

        if trimmed.is_empty() || trimmed.starts_with("/w ") {
            self.record_undo_state(split_id);
            let input_text = self.input_texts.entry(split_id).or_default();
            *input_text = format!("/w {} ", username);
            self.cursor_positions.insert(split_id, input_text.len());
            self.selection_ranges.remove(&split_id);
            self.reset_history_navigation(split_id);
            true
        } else {
            false
        }
    }

    /// Leave history navigation after the recalled text was edited. The draft
    /// stashed by the first Up comes back on Down only while the text still
    /// matches the recalled entry.
    pub fn end_history_navigation(&mut self, split_id: SplitId) {
        self.history_index.remove(&split_id);
    }

    /// Begin a marked-text transaction. Repeated preedit replacements use the
    /// captured baseline and become one undo step only when committed.
    pub(crate) fn begin_ime_composition(&mut self, split_id: SplitId) {
        if self.ime_composition_starts.contains_key(&split_id) {
            return;
        }
        self.break_edit_transaction(split_id);
        let baseline = self.snapshot(split_id);
        self.ime_composition_starts.insert(split_id, baseline);
    }

    /// Commit a marked-text transaction as one undoable draft edit.
    pub(crate) fn commit_ime_composition(&mut self, split_id: SplitId) -> bool {
        let Some(baseline) = self.ime_composition_starts.remove(&split_id) else {
            return false;
        };
        self.break_edit_transaction(split_id);
        if !self.snapshot_differs(split_id, &baseline) {
            return false;
        }
        self.record_undo_snapshot(split_id, baseline);
        true
    }

    /// Cancel a marked-text transaction and restore its exact prior draft
    /// state without disturbing the existing undo or redo stacks.
    pub(crate) fn cancel_ime_composition(&mut self, split_id: SplitId) -> bool {
        let Some(baseline) = self.ime_composition_starts.remove(&split_id) else {
            return false;
        };
        self.break_edit_transaction(split_id);
        let changed = self.snapshot_differs(split_id, &baseline);
        self.restore_snapshot(split_id, baseline);
        changed
    }

    /// Navigate to previous message in history (up arrow)
    pub fn history_up(&mut self, channel: &str, split_id: SplitId) -> Option<String> {
        self.break_edit_transaction(split_id);
        if self
            .input_history
            .get(channel)
            .is_none_or(|history| history.is_empty())
        {
            return None;
        }

        let current_index = self.history_index.get(&split_id).copied().flatten();
        if current_index.is_none_or(|idx| idx > 0) {
            self.record_undo_state(split_id);
        }
        let channel_history = self.input_history.get(channel)?;

        match current_index {
            None => {
                let current_text = self.input_texts.get(&split_id).cloned().unwrap_or_default();
                let unchanged_recall = self.temp_input.contains_key(&split_id)
                    && self.recalled_text.get(&split_id) == Some(&current_text);
                if !unchanged_recall {
                    self.temp_input.insert(split_id, Some(current_text));
                }
                let last_idx = channel_history.len() - 1;
                self.history_index.insert(split_id, Some(last_idx));
                let text = channel_history[last_idx].clone();
                self.recalled_text.insert(split_id, text.clone());
                self.input_texts.insert(split_id, text.clone());
                self.cursor_positions.insert(split_id, text.len());
                self.selection_ranges.remove(&split_id);
                Some(text)
            }
            Some(idx) if idx > 0 => {
                let new_idx = idx - 1;
                self.history_index.insert(split_id, Some(new_idx));
                let text = channel_history[new_idx].clone();
                self.recalled_text.insert(split_id, text.clone());
                self.input_texts.insert(split_id, text.clone());
                self.cursor_positions.insert(split_id, text.len());
                self.selection_ranges.remove(&split_id);
                Some(text)
            }
            _ => None,
        }
    }

    /// Navigate to next message in history (down arrow)
    pub fn history_down(&mut self, channel: &str, split_id: SplitId) -> Option<String> {
        self.break_edit_transaction(split_id);
        let current_index = self.history_index.get(&split_id).copied().flatten();
        if current_index.is_none() {
            let current_text = self
                .input_texts
                .get(&split_id)
                .map(String::as_str)
                .unwrap_or_default();
            if !self.temp_input.contains_key(&split_id)
                || self
                    .recalled_text
                    .get(&split_id)
                    .is_some_and(|recalled| recalled != current_text)
            {
                return None;
            }
        }
        self.record_undo_state(split_id);
        let channel_history = self.input_history.get(channel)?;

        match current_index {
            Some(idx) => {
                if idx + 1 < channel_history.len() {
                    let new_idx = idx + 1;
                    self.history_index.insert(split_id, Some(new_idx));
                    let text = channel_history[new_idx].clone();
                    self.recalled_text.insert(split_id, text.clone());
                    self.input_texts.insert(split_id, text.clone());
                    self.cursor_positions.insert(split_id, text.len());
                    self.selection_ranges.remove(&split_id);
                    Some(text)
                } else {
                    self.history_index.insert(split_id, None);
                    self.recalled_text.remove(&split_id);
                    let restored = self
                        .temp_input
                        .remove(&split_id)
                        .flatten()
                        .unwrap_or_default();
                    self.input_texts.insert(split_id, restored.clone());
                    self.cursor_positions.insert(split_id, restored.len());
                    self.selection_ranges.remove(&split_id);
                    Some(restored)
                }
            }
            None => {
                self.recalled_text.remove(&split_id);
                let restored = self.temp_input.remove(&split_id)?.unwrap_or_default();
                self.input_texts.insert(split_id, restored.clone());
                self.cursor_positions.insert(split_id, restored.len());
                self.selection_ranges.remove(&split_id);
                Some(restored)
            }
        }
    }

    /// Clean up input state for splits that no longer exist in the layout
    /// This prevents memory leaks when splits are rearranged or closed
    pub fn cleanup_split_ids(&mut self, valid_ids: &std::collections::HashSet<SplitId>) {
        self.input_texts
            .retain(|split_id, _| valid_ids.contains(split_id));
        self.cursor_positions
            .retain(|split_id, _| valid_ids.contains(split_id));
        self.selection_ranges
            .retain(|split_id, _| valid_ids.contains(split_id));
        self.undo_stacks
            .retain(|split_id, _| valid_ids.contains(split_id));
        self.edit_transactions
            .retain(|split_id, _| valid_ids.contains(split_id));
        self.ime_composition_starts
            .retain(|split_id, _| valid_ids.contains(split_id));
        if self
            .focused_split
            .is_some_and(|split_id| !valid_ids.contains(&split_id))
        {
            self.focused_split = None;
        }
        self.redo_stacks
            .retain(|split_id, _| valid_ids.contains(split_id));
        self.history_index
            .retain(|split_id, _| valid_ids.contains(split_id));
        self.temp_input
            .retain(|split_id, _| valid_ids.contains(split_id));
        self.recalled_text
            .retain(|split_id, _| valid_ids.contains(split_id));
        self.placeholder_inputs
            .retain(|split_id, _| valid_ids.contains(split_id));
        if self
            .focused_placeholder
            .is_some_and(|split_id| !valid_ids.contains(&split_id))
        {
            self.focused_placeholder = None;
        }
    }

    fn clamp_cursor(text: &str, cursor: usize) -> usize {
        if cursor >= text.len() {
            return text.len();
        }
        let mut last_boundary = 0;
        for (idx, _) in text.grapheme_indices(true) {
            if idx == cursor {
                return cursor;
            }
            if idx > cursor {
                return last_boundary;
            }
            last_boundary = idx;
        }
        last_boundary
    }

    fn snapshot(&self, split_id: SplitId) -> DraftEditSnapshot {
        DraftEditSnapshot {
            text: self.input_texts.get(&split_id).cloned().unwrap_or_default(),
            cursor: self.cursor_for_split(split_id),
            selection: self.selection_for_split(split_id),
        }
    }

    fn break_edit_transaction(&mut self, split_id: SplitId) {
        self.edit_transactions.remove(&split_id);
    }

    fn record_grouped_edit(&mut self, split_id: SplitId, kind: DraftEditKind) {
        let cursor = self.cursor_for_split(split_id);
        let len = self.text_for_split(split_id).len();
        let continuation = self
            .edit_transactions
            .get(&split_id)
            .is_some_and(|transaction| {
                transaction.kind == kind
                    && transaction.next_cursor == cursor
                    && transaction.next_len == len
                    && transaction.edited_at.elapsed() < DRAFT_EDIT_PAUSE
            });
        if !continuation {
            self.record_undo_state(split_id);
        }
    }

    fn finish_grouped_edit(&mut self, split_id: SplitId, kind: DraftEditKind) {
        self.edit_transactions.insert(
            split_id,
            DraftEditTransaction {
                kind,
                next_cursor: self.cursor_for_split(split_id),
                next_len: self.text_for_split(split_id).len(),
                edited_at: Instant::now(),
            },
        );
    }

    fn record_undo_state(&mut self, split_id: SplitId) {
        self.commit_ime_composition(split_id);
        self.break_edit_transaction(split_id);
        let snapshot = self.snapshot(split_id);
        self.record_undo_snapshot(split_id, snapshot);
    }

    fn record_undo_snapshot(&mut self, split_id: SplitId, snapshot: DraftEditSnapshot) {
        let stack = self.undo_stacks.entry(split_id).or_default();
        let is_duplicate = stack.last().is_some_and(|last| {
            last.text == snapshot.text
                && last.cursor == snapshot.cursor
                && last.selection == snapshot.selection
        });
        if !is_duplicate {
            Self::push_history_snapshot(stack, snapshot);
        }
        self.redo_stacks.remove(&split_id);
    }

    fn snapshot_differs(&self, split_id: SplitId, baseline: &DraftEditSnapshot) -> bool {
        let current = self.snapshot(split_id);
        current.text != baseline.text
            || current.cursor != baseline.cursor
            || current.selection != baseline.selection
    }

    fn push_history_snapshot(stack: &mut Vec<DraftEditSnapshot>, snapshot: DraftEditSnapshot) {
        stack.push(snapshot);
        let mut bytes: usize = stack.iter().map(|entry| entry.text.len()).sum();
        while stack.len() > MAX_DRAFT_UNDO_HISTORY || bytes > MAX_DRAFT_UNDO_BYTES {
            bytes -= stack.remove(0).text.len();
        }
    }

    fn restore_snapshot(&mut self, split_id: SplitId, snapshot: DraftEditSnapshot) {
        let cursor = Self::clamp_cursor(&snapshot.text, snapshot.cursor);
        let selection = snapshot.selection.and_then(|range| {
            (range.start < range.end
                && range.end <= snapshot.text.len()
                && snapshot.text.is_char_boundary(range.start)
                && snapshot.text.is_char_boundary(range.end))
            .then_some(range)
        });
        self.input_texts.insert(split_id, snapshot.text);
        self.cursor_positions.insert(split_id, cursor);
        if let Some(selection) = selection {
            self.selection_ranges.insert(split_id, selection);
        } else {
            self.selection_ranges.remove(&split_id);
        }
    }

    fn update_selection_from_anchor(
        &mut self,
        split_id: SplitId,
        selection: Option<Range<usize>>,
        cursor_before_move: usize,
        cursor_after_move: usize,
        extend_selection: bool,
    ) {
        if !extend_selection {
            self.selection_ranges.remove(&split_id);
            return;
        }

        let anchor = selection
            .map(|range| {
                if cursor_before_move == range.start {
                    range.end
                } else {
                    range.start
                }
            })
            .unwrap_or(cursor_before_move);
        if anchor == cursor_after_move {
            self.selection_ranges.remove(&split_id);
        } else {
            self.selection_ranges.insert(
                split_id,
                anchor.min(cursor_after_move)..anchor.max(cursor_after_move),
            );
        }
    }

    // Editing callers already canonicalize the cursor with cursor_for_split.
    fn previous_grapheme_boundary(text: &str, cursor: usize) -> usize {
        text.grapheme_indices(true)
            .take_while(|(idx, _)| *idx < cursor)
            .map(|(idx, _)| idx)
            .last()
            .unwrap_or(0)
    }

    fn next_grapheme_boundary(text: &str, cursor: usize) -> usize {
        if cursor >= text.len() {
            return text.len();
        }
        text.grapheme_indices(true)
            .map(|(idx, _)| idx)
            .find(|idx| *idx > cursor)
            .unwrap_or(text.len())
    }

    fn previous_word_boundary(text: &str, cursor: usize) -> usize {
        if cursor == 0 {
            return 0;
        }

        let chars: Vec<(usize, char)> = text.char_indices().collect();
        let mut idx = chars.partition_point(|(byte, _)| *byte < cursor);

        while idx > 0 && chars[idx - 1].1.is_whitespace() {
            idx -= 1;
        }
        while idx > 0 && !chars[idx - 1].1.is_whitespace() {
            idx -= 1;
        }

        chars.get(idx).map(|(byte, _)| *byte).unwrap_or(0)
    }

    fn next_word_boundary(text: &str, cursor: usize) -> usize {
        if cursor >= text.len() {
            return text.len();
        }

        let chars: Vec<(usize, char)> = text.char_indices().collect();
        let mut idx = chars.partition_point(|(byte, _)| *byte < cursor);

        while idx < chars.len() && chars[idx].1.is_whitespace() {
            idx += 1;
        }
        while idx < chars.len() && !chars[idx].1.is_whitespace() {
            idx += 1;
        }

        chars.get(idx).map(|(byte, _)| *byte).unwrap_or(text.len())
    }
}
