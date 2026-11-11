//! Composer input state.
//!
//! Handles:
//! - Per-split draft text, caret, selection and undo history
//! - Message history navigation (up/down arrows)
//! - Placeholder input management
//!
//! Reply state is per split in `SplitPane`; focus stays in `EchoApp` because
//! `FocusHandle` requires an `App` context.

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

/// Everything the composer keeps for one split's draft.
///
/// One struct per split means a split's draft lifecycle (clear, close, prune)
/// is a single map operation instead of one per field.
#[derive(Default)]
struct SplitDraft {
    /// `None` until the split has a draft; `Some("")` is a draft that was emptied.
    text: Option<String>,
    /// Caret byte offset. `None` keeps the caret at the end of the text.
    cursor: Option<usize>,
    selection: Option<Range<usize>>,
    edit_transaction: Option<DraftEditTransaction>,
    undo: Vec<DraftEditSnapshot>,
    redo: Vec<DraftEditSnapshot>,
    ime_composition_start: Option<DraftEditSnapshot>,
    /// Position in the channel's sent-message history while navigating it.
    history_index: Option<usize>,
    /// The channel `history_index` points into. The index is per split and
    /// the history per channel, and a Watching split changes channel in place.
    history_channel: Option<String>,
    /// The draft stashed by the first Up, restored by Down past the newest entry.
    stashed_input: Option<String>,
    /// The history entry last recalled into the draft.
    recalled_text: Option<String>,
}

impl SplitDraft {
    /// Replace the draft with `text`, caret at its end and no selection.
    fn show_text(&mut self, text: String) {
        self.cursor = Some(text.len());
        self.text = Some(text);
        self.selection = None;
    }

    /// Show a history entry and remember it as the recalled text.
    fn show_recalled(&mut self, text: String) {
        self.recalled_text = Some(text.clone());
        self.show_text(text);
    }
}

/// Whether `range` is a non-empty selection on UTF-8 boundaries of `text`.
fn is_selection_in(text: &str, range: &Range<usize>) -> bool {
    range.start < range.end
        && range.end <= text.len()
        && text.is_char_boundary(range.start)
        && text.is_char_boundary(range.end)
}

/// Composer input state for every split.
///
/// The focus handle stays in `EchoApp` because `FocusHandle` needs an `App`
/// context; reply state is per split in `SplitPane`. Empty splits keep their
/// channel chooser in `placeholder_inputs`, which has its own lifecycle.
#[derive(Default)]
pub struct InputManager {
    drafts: HashMap<SplitId, SplitDraft>,
    focused_split: Option<SplitId>,

    pub input_history: HashMap<String, Vec<String>>,

    pub placeholder_inputs: HashMap<SplitId, PlaceholderInput>,
    pub focused_placeholder: Option<SplitId>,
}

impl InputManager {
    pub fn new() -> Self {
        Self::default()
    }

    fn slot(&self, split_id: SplitId) -> Option<&SplitDraft> {
        self.drafts.get(&split_id)
    }

    fn slot_mut(&mut self, split_id: SplitId) -> &mut SplitDraft {
        self.drafts.entry(split_id).or_default()
    }

    /// The split's draft text, or `None` when it has no draft.
    pub fn draft(&self, split_id: SplitId) -> Option<&String> {
        self.slot(split_id)?.text.as_ref()
    }

    /// Whether the split has a draft, even an empty one.
    pub fn has_draft(&self, split_id: SplitId) -> bool {
        self.draft(split_id).is_some()
    }

    /// Replace the draft text without touching the caret, selection or undo history.
    pub fn set_draft_text(&mut self, split_id: SplitId, text: String) {
        self.slot_mut(split_id).text = Some(text);
    }

    /// Drop the draft text without touching the caret, selection or undo history.
    pub fn remove_draft_text(&mut self, split_id: SplitId) {
        if let Some(draft) = self.drafts.get_mut(&split_id) {
            draft.text = None;
        }
    }

    /// The caret exactly as stored, `None` when it tracks the end of the draft.
    pub fn stored_cursor(&self, split_id: SplitId) -> Option<usize> {
        self.slot(split_id)?.cursor
    }

    /// Leave history navigation and forget the stashed draft, keeping the
    /// recalled entry.
    pub fn drop_history_stash(&mut self, split_id: SplitId) {
        if let Some(draft) = self.drafts.get_mut(&split_id) {
            draft.history_index = None;
            draft.stashed_input = None;
        }
    }

    /// Current draft text for a split, or `""` when no draft exists.
    pub fn text_for_split(&self, split_id: SplitId) -> &str {
        self.draft(split_id).map(String::as_str).unwrap_or("")
    }

    pub fn cursor_for_split(&self, split_id: SplitId) -> usize {
        let text = self.text_for_split(split_id);
        let cursor = self
            .slot(split_id)
            .and_then(|draft| draft.cursor)
            .unwrap_or(text.len());
        Self::clamp_cursor(text, cursor)
    }

    /// Current non-empty UTF-8-safe selection for a composer draft.
    pub fn selection_for_split(&self, split_id: SplitId) -> Option<Range<usize>> {
        let range = self.slot(split_id)?.selection.clone()?;
        is_selection_in(self.text_for_split(split_id), &range).then_some(range)
    }

    pub fn set_cursor_for_split(&mut self, split_id: SplitId, cursor: usize) {
        self.break_edit_transaction(split_id);
        let cursor = Self::clamp_cursor(self.text_for_split(split_id), cursor);
        let draft = self.slot_mut(split_id);
        draft.cursor = Some(cursor);
        draft.selection = None;
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
        let text = self.text_for_split(split_id);
        let cursor = cursor.map(|cursor| Self::clamp_cursor(text, cursor));
        let selection = selection.filter(|range| is_selection_in(text, range));
        let draft = self.slot_mut(split_id);
        draft.cursor = cursor;
        draft.selection = selection;
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
        let text = self.text_for_split(split_id);
        let anchor = Self::clamp_cursor(text, anchor);
        let focus = Self::clamp_cursor(text, focus);
        let previous_cursor = self.cursor_for_split(split_id);
        let previous_selection = self.selection_for_split(split_id);
        let next_selection = (anchor != focus).then_some(anchor.min(focus)..anchor.max(focus));

        let draft = self.slot_mut(split_id);
        draft.cursor = Some(focus);
        draft.selection = next_selection.clone();

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
        let draft = self.slot_mut(split_id);
        let text = draft.text.get_or_insert_default();
        let next_cursor = if let Some(range) = selection {
            text.replace_range(range.clone(), inserted);
            range.start + inserted.len()
        } else {
            text.insert_str(cursor, inserted);
            cursor + inserted.len()
        };
        draft.cursor = Some(next_cursor);
        draft.selection = None;
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
        let text = self.text_for_split(split_id);
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
        let draft = self.slot_mut(split_id);
        draft
            .text
            .get_or_insert_default()
            .replace_range(range.clone(), inserted);
        let inserted_range = range.start..range.start + inserted.len();
        draft.cursor = Some(inserted_range.end);
        draft.selection = None;
        if record_undo {
            draft.redo.clear();
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

        let current_text = self.text_for_split(split_id);
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
        let draft = self.slot_mut(split_id);
        draft
            .text
            .get_or_insert_default()
            .replace_range(range, &replacement);
        draft.cursor = Some(cursor);
        draft.selection = None;
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
        self.slot_mut(split_id).cursor = Some(next);
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
            let draft = self.slot_mut(split_id);
            draft.cursor = Some(range.start);
            draft.selection = None;
            return true;
        }
        let text = self.text_for_split(split_id);
        let next = if by_word {
            Self::previous_word_boundary(text, cursor)
        } else {
            Self::previous_grapheme_boundary(text, cursor)
        };
        if next != cursor {
            self.slot_mut(split_id).cursor = Some(next);
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
            let draft = self.slot_mut(split_id);
            draft.cursor = Some(range.end);
            draft.selection = None;
            return true;
        }
        let text = self.text_for_split(split_id);
        let next = if by_word {
            Self::next_word_boundary(text, cursor)
        } else {
            Self::next_grapheme_boundary(text, cursor)
        };
        if next != cursor {
            self.slot_mut(split_id).cursor = Some(next);
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
        let draft = self.slot_mut(split_id);
        let text = draft.text.get_or_insert_default();
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
        draft.cursor = Some(start);
        draft.selection = None;
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
        let text_len = self.text_for_split(split_id).len();
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
        let draft = self.slot_mut(split_id);
        let text = draft.text.get_or_insert_default();
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
        draft.cursor = Some(next_cursor);
        draft.selection = None;
        if grouped {
            self.finish_grouped_edit(split_id, kind);
        }
        self.end_history_navigation(split_id);
        true
    }

    /// Select the word (or whitespace run) containing `offset` — the native
    /// double-click contract. Returns false when nothing changes.
    pub fn select_word_at(&mut self, split_id: SplitId, offset: usize) -> bool {
        self.break_edit_transaction(split_id);
        let Some(text) = self.draft(split_id) else {
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
        let draft = self.slot_mut(split_id);
        draft.cursor = Some(range.end);
        draft.selection = Some(range);
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

    /// Select the entire draft, matching Ctrl/Cmd+A behavior in a native input.
    pub fn select_all(&mut self, split_id: SplitId) -> bool {
        self.break_edit_transaction(split_id);
        let text_len = self.text_for_split(split_id).len();
        if text_len == 0 {
            return false;
        }
        let previous = self.selection_for_split(split_id);
        if previous.as_ref() == Some(&(0..text_len)) && self.cursor_for_split(split_id) == text_len
        {
            return false;
        }
        let draft = self.slot_mut(split_id);
        draft.selection = Some(0..text_len);
        draft.cursor = Some(text_len);
        true
    }

    /// Copy the active selection without changing the draft.
    pub fn copy_selection(&self, split_id: SplitId) -> Option<String> {
        let range = self.selection_for_split(split_id)?;
        self.draft(split_id)
            .map(|text| text[range].to_string())
            .filter(|text| !text.is_empty())
    }

    /// Cut the active selection and return the copied text.
    pub fn cut_selection(&mut self, split_id: SplitId) -> Option<String> {
        let range = self.selection_for_split(split_id)?;
        let copied = self
            .draft(split_id)
            .map(|text| text[range.clone()].to_string())?;
        if copied.is_empty() {
            return None;
        }
        self.record_undo_state(split_id);
        let draft = self.slot_mut(split_id);
        if let Some(text) = draft.text.as_mut() {
            text.replace_range(range.clone(), "");
        }
        draft.cursor = Some(range.start);
        draft.selection = None;
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
        let text = self.draft(split_id)?;
        if text.get(range.clone()) != Some(expected_text) {
            return None;
        }
        let removed = expected_text.to_string();
        self.record_undo_state(split_id);
        let draft = self.drafts.get_mut(&split_id)?;
        draft.text.as_mut()?.replace_range(range.clone(), "");
        draft.cursor = Some(range.start);
        draft.selection = None;
        Some(removed)
    }

    pub fn can_undo(&self, split_id: SplitId) -> bool {
        self.slot(split_id)
            .is_some_and(|draft| !draft.undo.is_empty())
    }

    pub fn can_redo(&self, split_id: SplitId) -> bool {
        self.slot(split_id)
            .is_some_and(|draft| !draft.redo.is_empty())
    }

    /// Restore the previous split-local draft edit, if any.
    pub fn undo(&mut self, split_id: SplitId) -> bool {
        if self.cancel_ime_composition(split_id) {
            return true;
        }
        self.break_edit_transaction(split_id);
        let Some(snapshot) = self
            .drafts
            .get_mut(&split_id)
            .and_then(|draft| draft.undo.pop())
        else {
            return false;
        };
        let current = self.snapshot(split_id);
        Self::push_history_snapshot(&mut self.slot_mut(split_id).redo, current);
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
            .drafts
            .get_mut(&split_id)
            .and_then(|draft| draft.redo.pop())
        else {
            return false;
        };
        let current = self.snapshot(split_id);
        Self::push_history_snapshot(&mut self.slot_mut(split_id).undo, current);
        self.restore_snapshot(split_id, snapshot);
        true
    }

    /// Clear the split's whole draft: text, caret, undo history and history
    /// navigation. Channel history and placeholder-channel state are owned by
    /// their own lifecycle paths and stay.
    pub fn clear_draft_for_split(&mut self, split_id: SplitId) {
        self.drafts.remove(&split_id);
    }

    /// Forget the history position and the stashed draft, e.g. when the
    /// split's channel changes.
    pub fn reset_history_navigation(&mut self, split_id: SplitId) {
        if let Some(draft) = self.drafts.get_mut(&split_id) {
            draft.edit_transaction = None;
            draft.history_index = None;
            draft.stashed_input = None;
            draft.recalled_text = None;
        }
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
        let trimmed = self.text_for_split(split_id).trim_start();

        if trimmed.is_empty() || trimmed.starts_with("/w ") {
            self.record_undo_state(split_id);
            self.slot_mut(split_id)
                .show_text(format!("/w {} ", username));
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
        if let Some(draft) = self.drafts.get_mut(&split_id) {
            draft.history_index = None;
        }
    }

    /// Begin a marked-text transaction. Repeated preedit replacements use the
    /// captured baseline and become one undo step only when committed.
    pub(crate) fn begin_ime_composition(&mut self, split_id: SplitId) {
        if self
            .slot(split_id)
            .is_some_and(|draft| draft.ime_composition_start.is_some())
        {
            return;
        }
        self.break_edit_transaction(split_id);
        let baseline = self.snapshot(split_id);
        self.slot_mut(split_id).ime_composition_start = Some(baseline);
    }

    /// Commit a marked-text transaction as one undoable draft edit.
    pub(crate) fn commit_ime_composition(&mut self, split_id: SplitId) -> bool {
        let Some(baseline) = self.take_ime_composition_start(split_id) else {
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
        let Some(baseline) = self.take_ime_composition_start(split_id) else {
            return false;
        };
        self.break_edit_transaction(split_id);
        let changed = self.snapshot_differs(split_id, &baseline);
        self.restore_snapshot(split_id, baseline);
        changed
    }

    fn take_ime_composition_start(&mut self, split_id: SplitId) -> Option<DraftEditSnapshot> {
        self.drafts.get_mut(&split_id)?.ime_composition_start.take()
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

        let history_len = self.input_history.get(channel)?.len();
        let current_index = self.history_index_in(channel, split_id, history_len);
        if current_index.is_none_or(|idx| idx > 0) {
            self.record_undo_state(split_id);
        }
        let current_text = self.text_for_split(split_id).to_string();
        let channel_history = self.input_history.get(channel)?;
        let draft = self.drafts.entry(split_id).or_default();

        match current_index {
            None => {
                let unchanged_recall = draft.stashed_input.is_some()
                    && draft.recalled_text.as_ref() == Some(&current_text);
                if !unchanged_recall {
                    draft.stashed_input = Some(current_text);
                }
                let last_idx = channel_history.len() - 1;
                draft.history_index = Some(last_idx);
                draft.history_channel = Some(channel.to_string());
                let text = channel_history[last_idx].clone();
                draft.show_recalled(text.clone());
                Some(text)
            }
            Some(idx) if idx > 0 => {
                let new_idx = idx - 1;
                draft.history_index = Some(new_idx);
                let text = channel_history[new_idx].clone();
                draft.show_recalled(text.clone());
                Some(text)
            }
            _ => None,
        }
    }

    /// The split's history position, if it is navigating `channel`'s history.
    /// A position left over from another channel counts as not navigating.
    fn history_index_in(
        &self,
        channel: &str,
        split_id: SplitId,
        history_len: usize,
    ) -> Option<usize> {
        let draft = self.slot(split_id)?;
        draft
            .history_index
            .filter(|idx| *idx < history_len && draft.history_channel.as_deref() == Some(channel))
    }

    /// Navigate to next message in history (down arrow)
    pub fn history_down(&mut self, channel: &str, split_id: SplitId) -> Option<String> {
        self.break_edit_transaction(split_id);
        let history_len = self.input_history.get(channel).map_or(0, Vec::len);
        let current_index = self.history_index_in(channel, split_id, history_len);
        if current_index.is_none() {
            let current_text = self.text_for_split(split_id);
            let can_restore = self.slot(split_id).is_some_and(|draft| {
                draft.stashed_input.is_some()
                    && draft
                        .recalled_text
                        .as_deref()
                        .is_none_or(|recalled| recalled == current_text)
            });
            if !can_restore {
                return None;
            }
        }
        self.record_undo_state(split_id);
        let channel_history = self.input_history.get(channel)?;
        let draft = self.drafts.entry(split_id).or_default();

        match current_index {
            Some(idx) => {
                if idx + 1 < channel_history.len() {
                    let new_idx = idx + 1;
                    draft.history_index = Some(new_idx);
                    let text = channel_history[new_idx].clone();
                    draft.show_recalled(text.clone());
                    Some(text)
                } else {
                    draft.history_index = None;
                    draft.recalled_text = None;
                    let restored = draft.stashed_input.take().unwrap_or_default();
                    draft.show_text(restored.clone());
                    Some(restored)
                }
            }
            None => {
                draft.history_index = None;
                draft.recalled_text = None;
                let restored = draft.stashed_input.take()?;
                draft.show_text(restored.clone());
                Some(restored)
            }
        }
    }

    /// Clean up input state for splits that no longer exist in the layout
    /// This prevents memory leaks when splits are rearranged or closed
    pub fn cleanup_split_ids(&mut self, valid_ids: &std::collections::HashSet<SplitId>) {
        self.drafts
            .retain(|split_id, _| valid_ids.contains(split_id));
        if self
            .focused_split
            .is_some_and(|split_id| !valid_ids.contains(&split_id))
        {
            self.focused_split = None;
        }
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
            text: self.text_for_split(split_id).to_string(),
            cursor: self.cursor_for_split(split_id),
            selection: self.selection_for_split(split_id),
        }
    }

    fn break_edit_transaction(&mut self, split_id: SplitId) {
        if let Some(draft) = self.drafts.get_mut(&split_id) {
            draft.edit_transaction = None;
        }
    }

    fn record_grouped_edit(&mut self, split_id: SplitId, kind: DraftEditKind) {
        let cursor = self.cursor_for_split(split_id);
        let len = self.text_for_split(split_id).len();
        let continuation = self
            .slot(split_id)
            .and_then(|draft| draft.edit_transaction.as_ref())
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
        let transaction = DraftEditTransaction {
            kind,
            next_cursor: self.cursor_for_split(split_id),
            next_len: self.text_for_split(split_id).len(),
            edited_at: Instant::now(),
        };
        self.slot_mut(split_id).edit_transaction = Some(transaction);
    }

    fn record_undo_state(&mut self, split_id: SplitId) {
        self.commit_ime_composition(split_id);
        self.break_edit_transaction(split_id);
        let snapshot = self.snapshot(split_id);
        self.record_undo_snapshot(split_id, snapshot);
    }

    fn record_undo_snapshot(&mut self, split_id: SplitId, snapshot: DraftEditSnapshot) {
        let draft = self.slot_mut(split_id);
        let is_duplicate = draft.undo.last().is_some_and(|last| {
            last.text == snapshot.text
                && last.cursor == snapshot.cursor
                && last.selection == snapshot.selection
        });
        if !is_duplicate {
            Self::push_history_snapshot(&mut draft.undo, snapshot);
        }
        draft.redo.clear();
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
        let DraftEditSnapshot {
            text,
            cursor,
            selection,
        } = snapshot;
        let cursor = Self::clamp_cursor(&text, cursor);
        let selection = selection.filter(|range| is_selection_in(&text, range));
        let draft = self.slot_mut(split_id);
        draft.text = Some(text);
        draft.cursor = Some(cursor);
        draft.selection = selection;
    }

    fn update_selection_from_anchor(
        &mut self,
        split_id: SplitId,
        selection: Option<Range<usize>>,
        cursor_before_move: usize,
        cursor_after_move: usize,
        extend_selection: bool,
    ) {
        let next = if extend_selection {
            let anchor = selection
                .map(|range| {
                    if cursor_before_move == range.start {
                        range.end
                    } else {
                        range.start
                    }
                })
                .unwrap_or(cursor_before_move);
            (anchor != cursor_after_move)
                .then(|| anchor.min(cursor_after_move)..anchor.max(cursor_after_move))
        } else {
            None
        };
        self.slot_mut(split_id).selection = next;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn history_up_survives_a_channel_with_a_shorter_history() {
        let mut manager = InputManager::new();
        let split_id = SplitId::new();
        manager.input_history.insert(
            "long".to_string(),
            vec!["one".into(), "two".into(), "three".into(), "four".into()],
        );
        manager
            .input_history
            .insert("short".to_string(), vec!["only".into()]);

        assert_eq!(
            manager.history_up("long", split_id).as_deref(),
            Some("four")
        );
        assert_eq!(
            manager.history_up("long", split_id).as_deref(),
            Some("three")
        );

        // The split now shows another channel, as a Watching split does when
        // the browser moves on; its index is past this history's end.
        assert_eq!(
            manager.history_up("short", split_id).as_deref(),
            Some("only")
        );
        assert_eq!(manager.history_up("short", split_id), None);

        // Back on the longer history the leftover index is in range, but it
        // belongs to the other channel: navigation starts from the newest.
        assert_eq!(
            manager.history_up("long", split_id).as_deref(),
            Some("four")
        );
    }
}
