//! Multi-message text selection
//!
//! Supports drag selection across multiple messages with:
//! - Character-level precision using LineLayout hit testing
//! - Selection highlighting during paint
//! - Clipboard copy (Ctrl+C)
#![allow(clippy::single_range_in_vec_init)] // Single-element vec is intentional API

use gpui::SharedString;
use smallvec::SmallVec;
use std::ops::Range;
use std::sync::Arc;

use echo_render_core::eviction_cache::EvictionTrackingCache;

use super::layout::{CanvasMessageLayout, LayoutCacheKey, LayoutSegment};
use crate::ports;

/// PERF: Selection ranges type - typically 0-2 ranges per message
/// Using SmallVec avoids heap allocation for common single-selection cases
pub type SelectionRanges = SmallVec<[Range<usize>; 2]>;

// ============================================================================
// Selection State
// ============================================================================

/// Global text selection state
#[derive(Clone, Debug, Default)]
pub struct SelectionState {
    /// Start of selection (anchor point - doesn't move during drag)
    pub anchor: Option<SelectionPoint>,
    /// Current end of selection (follows mouse)
    pub focus: Option<SelectionPoint>,
    /// Whether currently dragging to select
    pub is_dragging: bool,
}

/// A point in the selection (message + character position)
/// PERF: Uses Arc<str> for message_id to avoid String allocation on every mouse move
#[derive(Clone, Debug, PartialEq)]
pub struct SelectionPoint {
    /// Message ID (SharedString for cheap clone)
    pub message_id: SharedString,
    /// Index of this message in the visible list
    pub message_index: usize,
    /// Character index within the message's source text
    pub char_index: usize,
}

impl SelectionState {
    /// Create new empty selection state
    pub fn new() -> Self {
        Self::default()
    }

    /// Check if there is an active selection
    pub fn has_selection(&self) -> bool {
        self.anchor.is_some() && self.focus.is_some()
    }

    /// Start a new selection at the given point
    pub fn start_selection(&mut self, point: SelectionPoint) {
        self.anchor = Some(point.clone());
        self.focus = Some(point);
        self.is_dragging = true;
    }

    /// Extend selection to the given point.
    ///
    /// Returns whether the focus actually moved. A drag re-hits the same
    /// character for most of the pointer events it generates, and the caller
    /// repaints the whole window root on every reported change.
    pub fn extend_selection(&mut self, point: SelectionPoint) -> bool {
        if !self.is_dragging || self.focus.as_ref() == Some(&point) {
            return false;
        }
        self.focus = Some(point);
        true
    }

    /// Replace both endpoints during a word- or message-granular drag.
    /// Returns whether either endpoint changed.
    pub fn set_endpoints(&mut self, anchor: SelectionPoint, focus: SelectionPoint) -> bool {
        if !self.is_dragging
            || (self.anchor.as_ref() == Some(&anchor) && self.focus.as_ref() == Some(&focus))
        {
            return false;
        }
        self.anchor = Some(anchor);
        self.focus = Some(focus);
        true
    }

    /// End the current drag operation. Returns whether a drag was active.
    pub fn end_drag(&mut self) -> bool {
        let was_dragging = self.is_dragging;
        self.is_dragging = false;
        was_dragging
    }

    /// Clear the selection
    pub fn clear(&mut self) {
        self.anchor = None;
        self.focus = None;
        self.is_dragging = false;
    }

    /// Re-resolve both endpoints by message identity after the message order
    /// changed. Clears the selection when either endpoint's message is gone.
    /// Returns whether the selection state changed.
    pub fn remap_message_indices<F>(&mut self, mut resolve_index: F) -> bool
    where
        F: FnMut(&str) -> Option<usize>,
    {
        let (Some(anchor), Some(focus)) = (self.anchor.as_ref(), self.focus.as_ref()) else {
            return false;
        };
        let (Some(anchor_index), Some(focus_index)) = (
            resolve_index(anchor.message_id.as_ref()),
            resolve_index(focus.message_id.as_ref()),
        ) else {
            self.clear();
            return true;
        };
        if anchor.message_index == anchor_index && focus.message_index == focus_index {
            return false;
        }
        if let Some(anchor) = self.anchor.as_mut() {
            anchor.message_index = anchor_index;
        }
        if let Some(focus) = self.focus.as_mut() {
            focus.message_index = focus_index;
        }
        true
    }

    /// Get the selection range (normalized so start <= end)
    pub fn get_normalized_range(&self) -> Option<(SelectionPoint, SelectionPoint)> {
        let anchor = self.anchor.as_ref()?;
        let focus = self.focus.as_ref()?;

        // Compare by message index first, then char index
        let anchor_before =
            (anchor.message_index, anchor.char_index) <= (focus.message_index, focus.char_index);

        if anchor_before {
            Some((anchor.clone(), focus.clone()))
        } else {
            Some((focus.clone(), anchor.clone()))
        }
    }

    /// Get the selected text from layouts
    /// PERF: Uses Arc<CanvasMessageLayout> to avoid cloning large structs
    /// PERF: O(k) where k = selected messages - uses stored indices instead of linear scans
    pub fn get_selected_text(
        &self,
        layouts: &EvictionTrackingCache<LayoutCacheKey, Arc<CanvasMessageLayout>>,
        message_order: &[SharedString],
        container_width: f32,
        show_timestamp: bool,
    ) -> String {
        self.get_selected_text_with(message_order, |msg_id| {
            let cache_key =
                LayoutCacheKey::from_shared(msg_id.clone(), container_width, show_timestamp);
            layouts.peek(&cache_key).cloned()
        })
    }

    /// Get selected text through a caller-owned layout resolver.
    ///
    /// The canvas uses this to fall back to the same older-width layout it is
    /// visibly painting while an exact-width async layout is pending. Clipboard
    /// source text is width-independent, so a resize must not make otherwise
    /// visible system rows disappear from a multi-row copy.
    pub fn get_selected_text_with<F>(
        &self,
        message_order: &[SharedString],
        mut resolve_layout: F,
    ) -> String
    where
        F: FnMut(&SharedString) -> Option<Arc<CanvasMessageLayout>>,
    {
        let (start, end) = match self.get_normalized_range() {
            Some(range) => range,
            None => return String::new(),
        };

        let mut result = String::new();

        // PERF: Use stored indices directly (O(1) instead of 2 linear scans)
        let start_order_idx = start.message_index;
        let end_order_idx = end.message_index;

        // Only iterate over messages in the selection range
        #[allow(clippy::needless_range_loop)]
        // i is used for both indexing and boundary comparisons
        for i in start_order_idx..=end_order_idx.min(message_order.len().saturating_sub(1)) {
            let Some(msg_id) = message_order.get(i) else {
                break;
            };
            let layout = match resolve_layout(msg_id) {
                Some(layout) => layout,
                None => continue,
            };

            let source = &layout.source_text;

            if i == start_order_idx && i == end_order_idx {
                // Selection within single message
                // Convert char indices to byte indices for safe UTF-8 slicing
                let start_byte = source
                    .char_indices()
                    .nth(start.char_index)
                    .map(|(idx, _)| idx)
                    .unwrap_or(source.len());
                let end_byte = source
                    .char_indices()
                    .nth(end.char_index)
                    .map(|(idx, _)| idx)
                    .unwrap_or(source.len());
                result.push_str(&source[start_byte..end_byte]);
            } else if i == start_order_idx {
                // First message: from start char to end
                let start_byte = source
                    .char_indices()
                    .nth(start.char_index)
                    .map(|(idx, _)| idx)
                    .unwrap_or(source.len());
                result.push_str(&source[start_byte..]);
                result.push('\n');
            } else if i == end_order_idx {
                // Last message: from beginning to end char
                let end_byte = source
                    .char_indices()
                    .nth(end.char_index)
                    .map(|(idx, _)| idx)
                    .unwrap_or(source.len());
                result.push_str(&source[..end_byte]);
            } else {
                // Middle message: full text
                result.push_str(source);
                result.push('\n');
            }
        }

        result
    }

    /// Get character ranges to highlight for a specific message
    ///
    /// PERF: O(1) - uses pre-stored message indices instead of linear scans.
    /// PERF: Returns SmallVec<[Range; 2]> to avoid heap allocation (selections are typically 0-1 ranges)
    /// The message_index parameter is the caller-known index of this message
    /// in the visible message order.
    pub fn get_ranges_for_message(
        &self,
        message_index: usize,
        source_text_char_len: usize,
    ) -> SelectionRanges {
        let (start, end) = match self.get_normalized_range() {
            Some(range) => range,
            None => return SmallVec::new(),
        };

        // PERF: Use stored indices directly (O(1) instead of 3 linear scans)
        let start_idx = start.message_index;
        let end_idx = end.message_index;
        let msg_idx = message_index;

        // Check if this message is in the selection range
        if msg_idx < start_idx || msg_idx > end_idx {
            return SmallVec::new();
        }

        if msg_idx == start_idx && msg_idx == end_idx {
            // Single message selection
            let s = start.char_index.min(source_text_char_len);
            let e = end.char_index.min(source_text_char_len);
            if s < e {
                smallvec::smallvec![s..e]
            } else {
                SmallVec::new()
            }
        } else if msg_idx == start_idx {
            // First message in selection
            let s = start.char_index.min(source_text_char_len);
            smallvec::smallvec![s..source_text_char_len]
        } else if msg_idx == end_idx {
            // Last message in selection
            let e = end.char_index.min(source_text_char_len);
            smallvec::smallvec![0..e]
        } else {
            // Middle message: full selection
            smallvec::smallvec![0..source_text_char_len]
        }
    }

    /// Return whether a hit-tested insertion point is inside the current selection.
    pub fn contains_point(&self, message_index: usize, char_index: usize) -> bool {
        let (start, end) = match self.get_normalized_range() {
            Some(range) => range,
            None => return false,
        };

        let start_key = (start.message_index, start.char_index);
        let end_key = (end.message_index, end.char_index);
        let point_key = (message_index, char_index);

        start_key < end_key && start_key <= point_key && point_key <= end_key
    }

    /// Copy selected text to clipboard
    /// PERF: Uses Arc<CanvasMessageLayout> to avoid cloning large structs
    pub fn copy_to_clipboard(
        &self,
        layouts: &EvictionTrackingCache<LayoutCacheKey, Arc<CanvasMessageLayout>>,
        message_order: &[SharedString],
        container_width: f32,
        show_timestamp: bool,
    ) -> bool {
        let text = self.get_selected_text(layouts, message_order, container_width, show_timestamp);
        if !text.is_empty() {
            ports::copy_text(&text);
            true
        } else {
            false
        }
    }

    pub fn copy_to_clipboard_with<F>(
        &self,
        message_order: &[SharedString],
        resolve_layout: F,
    ) -> bool
    where
        F: FnMut(&SharedString) -> Option<Arc<CanvasMessageLayout>>,
    {
        let text = self.get_selected_text_with(message_order, resolve_layout);
        if !text.is_empty() {
            ports::copy_text(&text);
            true
        } else {
            false
        }
    }
}

/// Char range of the word at `char_index` in a message, for double-click
/// selection. An inline emote, plain or layered, is one word; other words are
/// whitespace-delimited runs of `source_text`, cut at adjoining emotes.
pub fn word_char_range_at(layout: &CanvasMessageLayout, char_index: usize) -> Range<usize> {
    let next_char_breaks = || {
        layout
            .source_text
            .chars()
            .nth(char_index)
            .is_none_or(char::is_whitespace)
    };
    if let Some(image) = image_source_ranges(layout).find(|image| {
        image.contains(&char_index) || (image.end == char_index && next_char_breaks())
    }) {
        return image;
    }
    let mut word = word_char_range_in_text(&layout.source_text, char_index);
    for image in image_source_ranges(layout) {
        if image.end <= char_index && image.end > word.start {
            word.start = image.end;
        }
        if image.start >= char_index && image.start < word.end {
            word.end = image.start;
        }
    }
    word
}

fn image_source_ranges(layout: &CanvasMessageLayout) -> impl Iterator<Item = Range<usize>> + '_ {
    layout
        .segments
        .iter()
        .filter_map(|segment| match segment {
            LayoutSegment::Image {
                source_range: Some((start, len)),
                ..
            } => Some(*start..*start + *len),
            LayoutSegment::LayeredImage {
                source_start_char,
                source_len_chars,
                ..
            } => Some(*source_start_char..*source_start_char + *source_len_chars),
            _ => None,
        })
        .filter(|range| !range.is_empty())
}

/// Char range of the whitespace-delimited word at the insertion point
/// `char_index`. A point just past a word's last char (a hit on that glyph's
/// right half) selects that word; a point inside a whitespace run selects the run.
pub fn word_char_range_in_text(text: &str, char_index: usize) -> Range<usize> {
    let mut index = 0;
    let mut previous = None;
    let mut at = None;
    for ch in text.chars() {
        if index == char_index {
            at = Some(ch);
            break;
        }
        previous = Some(ch);
        index += 1;
    }
    let (probe, in_space) = match (at, previous) {
        (Some(ch), _) if !ch.is_whitespace() => (index, false),
        (_, Some(ch)) if !ch.is_whitespace() => (index - 1, false),
        (Some(_), _) => (index, true),
        (None, Some(_)) => (index - 1, true),
        (None, None) => return index..index,
    };
    let mut start = 0;
    let mut len = 0;
    for (i, ch) in text.chars().enumerate() {
        len = i + 1;
        if ch.is_whitespace() != in_space {
            if i > probe {
                return start..i;
            }
            start = i + 1;
        }
    }
    start..len
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::num::NonZeroUsize;

    fn layout_with_text(message_id: &str, text: &str, channel: &str) -> Arc<CanvasMessageLayout> {
        let mut layout = CanvasMessageLayout::new(message_id.to_string(), channel);
        layout.source_text = text.to_string();
        layout.source_char_positions = (0..=text.chars().count()).map(|i| i as f32).collect();
        Arc::new(layout)
    }

    #[test]
    fn get_selected_text_multiline_multimessage_utf8() {
        let container_width = 320.0;
        let channel = "test-channel";
        let msg1_id = SharedString::from("m1");
        let msg2_id = SharedString::from("m2");

        let msg1_text = "A😊B\n中";
        let msg2_text = "γειά σου";

        let mut layouts: EvictionTrackingCache<LayoutCacheKey, Arc<CanvasMessageLayout>> =
            EvictionTrackingCache::new(NonZeroUsize::new(8).expect("non-zero cache capacity"));
        layouts.put(
            LayoutCacheKey::new(msg1_id.as_ref(), container_width, true),
            layout_with_text(msg1_id.as_ref(), msg1_text, channel),
        );
        layouts.put(
            LayoutCacheKey::new(msg2_id.as_ref(), container_width, true),
            layout_with_text(msg2_id.as_ref(), msg2_text, channel),
        );

        let message_order = vec![msg1_id.clone(), msg2_id.clone()];

        let mut selection = SelectionState::new();
        selection.start_selection(SelectionPoint {
            message_id: msg1_id,
            message_index: 0,
            char_index: 1, // starts at "😊"
        });
        selection.extend_selection(SelectionPoint {
            message_id: msg2_id,
            message_index: 1,
            char_index: 2, // first two Greek chars: "γε"
        });
        selection.end_drag();

        let selected = selection.get_selected_text(&layouts, &message_order, container_width, true);
        assert_eq!(selected, "😊B\n中\nγε");
    }

    #[test]
    fn get_selected_text_normalizes_reverse_drag_and_clamps_end_index() {
        let container_width = 320.0;
        let channel = "test-channel";
        let msg1_id = SharedString::from("m1");
        let msg2_id = SharedString::from("m2");

        let msg1_text = "hello";
        let msg2_text = "世界🙂";

        let mut layouts: EvictionTrackingCache<LayoutCacheKey, Arc<CanvasMessageLayout>> =
            EvictionTrackingCache::new(NonZeroUsize::new(8).expect("non-zero cache capacity"));
        layouts.put(
            LayoutCacheKey::new(msg1_id.as_ref(), container_width, true),
            layout_with_text(msg1_id.as_ref(), msg1_text, channel),
        );
        layouts.put(
            LayoutCacheKey::new(msg2_id.as_ref(), container_width, true),
            layout_with_text(msg2_id.as_ref(), msg2_text, channel),
        );

        let message_order = vec![msg1_id.clone(), msg2_id.clone()];

        let mut selection = SelectionState::new();
        selection.start_selection(SelectionPoint {
            message_id: msg2_id,
            message_index: 1,
            char_index: 99, // intentionally out of bounds: should clamp to end
        });
        selection.extend_selection(SelectionPoint {
            message_id: msg1_id,
            message_index: 0,
            char_index: 2,
        });
        selection.end_drag();

        let selected = selection.get_selected_text(&layouts, &message_order, container_width, true);
        assert_eq!(selected, "llo\n世界🙂");
    }

    fn point(message_id: &str, message_index: usize, char_index: usize) -> SelectionPoint {
        SelectionPoint {
            message_id: SharedString::from(message_id.to_string()),
            message_index,
            char_index,
        }
    }

    #[test]
    fn remap_message_indices_follows_rotated_message_identity() {
        let container_width = 320.0;
        let channel = "test-channel";
        let mut layouts: EvictionTrackingCache<LayoutCacheKey, Arc<CanvasMessageLayout>> =
            EvictionTrackingCache::new(NonZeroUsize::new(16).expect("non-zero cache capacity"));
        let ids: Vec<SharedString> = (0..8)
            .map(|i| SharedString::from(format!("m{i}")))
            .collect();
        for id in &ids {
            layouts.put(
                LayoutCacheKey::new(id.as_ref(), container_width, true),
                layout_with_text(id.as_ref(), &format!("text {id}"), channel),
            );
        }

        let mut selection = SelectionState::new();
        selection.start_selection(point("m5", 5, 0));
        selection.extend_selection(point("m6", 6, 4));
        selection.end_drag();
        let before = selection.get_selected_text(&layouts, &ids, container_width, true);

        let rotated: Vec<SharedString> = ids[3..].to_vec();
        let changed =
            selection.remap_message_indices(|id| rotated.iter().position(|m| m.as_ref() == id));
        assert!(changed);
        assert_eq!(selection.anchor.as_ref().map(|p| p.message_index), Some(2));
        assert_eq!(selection.focus.as_ref().map(|p| p.message_index), Some(3));
        let after = selection.get_selected_text(&layouts, &rotated, container_width, true);
        assert_eq!(before, after);
        assert_eq!(after, "text m5\ntext");

        let evicted: Vec<SharedString> = ids[6..].to_vec();
        assert!(
            selection.remap_message_indices(|id| evicted.iter().position(|m| m.as_ref() == id))
        );
        assert!(!selection.has_selection());
    }

    #[test]
    fn word_char_range_in_text_selects_word_under_pointer() {
        let text = "hello  wörld 😀";
        assert_eq!(word_char_range_in_text(text, 0), 0..5);
        assert_eq!(word_char_range_in_text(text, 3), 0..5);
        assert_eq!(word_char_range_in_text(text, 5), 0..5);
        assert_eq!(word_char_range_in_text(text, 6), 5..7);
        assert_eq!(word_char_range_in_text(text, 7), 7..12);
        assert_eq!(word_char_range_in_text(text, 12), 7..12);
        assert_eq!(word_char_range_in_text(text, 13), 13..14);
        assert_eq!(word_char_range_in_text(text, 99), 13..14);
        assert_eq!(word_char_range_in_text("", 0), 0..0);
    }

    #[test]
    fn word_char_range_at_treats_emote_code_as_one_word() {
        let mut layout = CanvasMessageLayout::new("m1".to_string(), "test-channel");
        layout.source_text = "user: Kappa:) hi".to_string();
        layout.segments.push(LayoutSegment::Image {
            url: SharedString::from("https://example.invalid/kappa.png"),
            x: 0.0,
            y: 0.0,
            width: 28.0,
            height: 28.0,
            is_animated: false,
            image_type: crate::layout::ImageType::Emote {
                name: SharedString::from("Kappa:)"),
                tooltip_meta: None,
            },
            overlays: Vec::new(),
            source_range: Some((6, 7)),
        });

        assert_eq!(word_char_range_at(&layout, 6), 6..13);
        assert_eq!(word_char_range_at(&layout, 9), 6..13);
        assert_eq!(word_char_range_at(&layout, 13), 6..13);
        assert_eq!(word_char_range_at(&layout, 15), 14..16);
        assert_eq!(word_char_range_at(&layout, 2), 0..5);
    }

    #[test]
    fn word_char_range_at_splits_emoji_from_adjoining_text() {
        let mut layout = CanvasMessageLayout::new("m1".to_string(), "test-channel");
        layout.source_text = "user: \u{1F600}hi".to_string();
        layout.segments.push(LayoutSegment::Image {
            url: SharedString::from("https://example.invalid/grin.png"),
            x: 0.0,
            y: 0.0,
            width: 20.0,
            height: 20.0,
            is_animated: false,
            image_type: crate::layout::ImageType::Emote {
                name: SharedString::from("\u{1F600}"),
                tooltip_meta: None,
            },
            overlays: Vec::new(),
            source_range: Some((6, 1)),
        });

        assert_eq!(word_char_range_at(&layout, 6), 6..7);
        assert_eq!(word_char_range_at(&layout, 7), 7..9);
        assert_eq!(word_char_range_at(&layout, 8), 7..9);
        assert_eq!(word_char_range_at(&layout, 9), 7..9);
    }

    #[test]
    fn get_selected_text_with_stale_selection_on_empty_order_is_empty() {
        let mut selection = SelectionState::new();
        selection.start_selection(point("m0", 0, 0));
        selection.extend_selection(point("m0", 0, 3));
        selection.end_drag();

        let selected = selection.get_selected_text_with(&[], |_| None);
        assert!(selected.is_empty());
    }
}
