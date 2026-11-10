//! Mixed-direction geometry probe for the feedback UI benchmark.
//!
//! When the benchmark fixture asks for it, the shared [`Input`](super::Input)
//! loads the fixture's mixed-direction text, and every prepaint of that text
//! publishes the caret, the live selection and a fixture selection whose
//! visual spans are discontiguous, together with interior x coordinates that
//! hit-test back to that selection's boundaries.

use gpui::{App, Bounds, CaretAffinity, Entity, Pixels, ShapedLine, Window, px};
use std::ops::Range;

use crate::bench::ui_fixture::{
    self as fixture, FeedbackCaretAffinity, FeedbackRect, FeedbackSharedInputSnapshot,
};

use super::state::InputState;

impl From<CaretAffinity> for FeedbackCaretAffinity {
    fn from(affinity: CaretAffinity) -> Self {
        match affinity {
            CaretAffinity::Leading => Self::Leading,
            CaretAffinity::Trailing => Self::Trailing,
        }
    }
}

/// One end of the fixture selection: its byte offset and an interior x
/// (relative to the line start) whose hit test resolves to that offset.
#[derive(Clone, Copy)]
struct HitTestBoundary {
    index: usize,
    x: f32,
    affinity: CaretAffinity,
}

#[derive(Clone, Copy)]
struct TargetSelection {
    start: HitTestBoundary,
    end: HitTestBoundary,
}

/// Replace the input's value with the fixture text.
pub(super) fn load_bidi_input_text(state: &Entity<InputState>, window: &mut Window, cx: &mut App) {
    state.update(cx, |state, cx| {
        state.set_value(fixture::FEEDBACK_BIDI_INPUT_TEXT, window, cx);
    });
}

/// Publish the painted geometry of the fixture text. `bounds` is the input's
/// canvas, `selection_bounds` the selection quads about to be painted.
pub(super) fn publish_bidi_input_probe(
    line: &ShapedLine,
    text: &str,
    bounds: Bounds<Pixels>,
    cursor: usize,
    cursor_affinity: CaretAffinity,
    selection: Option<&Range<usize>>,
    selection_bounds: &[Bounds<Pixels>],
) {
    let target = find_discontiguous_selection(line, text);
    fixture::publish_feedback_shared_input(FeedbackSharedInputSnapshot {
        bounds: feedback_rect(&bounds),
        text_len: text.len(),
        cursor,
        cursor_affinity: cursor_affinity.into(),
        target_selection_start: target.map(|target| target.start.index),
        target_selection_end: target.map(|target| target.end.index),
        target_selection_start_affinity: target.map(|target| target.start.affinity.into()),
        target_selection_end_affinity: target.map(|target| target.end.affinity.into()),
        selection_start_x: target.map(|target| target.start.x),
        selection_end_x: target.map(|target| target.end.x),
        selection_start: selection.map(|range| range.start),
        selection_end: selection.map(|range| range.end),
        selection_spans: selection_bounds.iter().map(feedback_rect).collect(),
    });
}

/// The first character-boundary range, in text order, that paints as two or
/// more non-empty visual spans and whose both ends can be hit-tested.
fn find_discontiguous_selection(line: &ShapedLine, text: &str) -> Option<TargetSelection> {
    let boundaries = text
        .char_indices()
        .map(|(offset, _)| offset)
        .chain(std::iter::once(text.len()))
        .collect::<Vec<_>>();
    boundaries
        .iter()
        .enumerate()
        .find_map(|(start_index, &start)| {
            boundaries[start_index + 1..].iter().find_map(|&end| {
                let spans = line.visual_spans_for_range(start..end);
                if spans.len() < 2 || !spans.iter().all(|span| span.end > span.start) {
                    return None;
                }
                Some(TargetSelection {
                    start: hit_test_boundary(line, start)?,
                    end: hit_test_boundary(line, end)?,
                })
            })
        })
}

/// The first whole-pixel x strictly inside the line whose hit test lands on
/// `index`.
fn hit_test_boundary(line: &ShapedLine, index: usize) -> Option<HitTestBoundary> {
    let line_width: f32 = line.width().into();
    let whole_pixels = line_width.floor() as i32;
    (1..whole_pixels).find_map(|step| {
        let x = px(step as f32);
        line.caret_for_x(x)
            .filter(|caret| caret.index == index)
            .map(|caret| HitTestBoundary {
                index,
                x: x.into(),
                affinity: caret.affinity,
            })
    })
}

fn feedback_rect(bounds: &Bounds<Pixels>) -> FeedbackRect {
    FeedbackRect {
        x: bounds.origin.x.into(),
        y: bounds.origin.y.into(),
        width: bounds.size.width.into(),
        height: bounds.size.height.into(),
    }
}
