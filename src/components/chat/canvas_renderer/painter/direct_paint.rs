//! Direct viewport paint command preparation and GPU paint helpers.

pub(super) use super::image_paint::{animated_cache_state_tag, image_load_state_tag};
use super::image_paint::{
    paint_images_direct, paint_prepared_images_direct, prepare_image_paint_commands,
};
use super::text_paint::{
    paint_prepared_text_direct, paint_text_direct, prepare_text_paint_commands,
};
use super::*;
use crate::components::chat::canvas_renderer::layout::{HighlightColors, LowTrustColors};

pub(super) use echo_chat_render::paint_order::{MessagePaintLayer, message_paint_layers};

#[derive(Clone)]
pub(super) struct ViewportPaintRow {
    pub(super) layout: Arc<CanvasMessageLayout>,
    pub(super) message_index: usize,
    pub(super) selection_ranges: SelectionRanges,
    pub(super) viewport_y: f32,
    pub(super) search_focused: bool,
    pub(super) is_alternate_row: bool,
    pub(super) text_commands: SmallVec<[PreparedTextPaintCommand; 4]>,
    pub(super) image_commands: SmallVec<[PreparedImagePaintCommand; 8]>,
}

#[derive(Clone)]
pub(super) struct PreparedTextPaintCommand {
    pub(super) line_idx: usize,
    pub(super) x: f32,
    pub(super) y: f32,
    /// `Rc` so reusing/cloning a prepared row is a refcount bump, not a deep
    /// `ShapedLine` glyph/run clone (thread-local cache, single paint thread).
    pub(super) shaped_line: Rc<ShapedLine>,
    /// Exact 7TV name-paint overlay, fully composited at prepare time. The paint
    /// path just derefs this `Option` — no COMPOSITE_CACHE mutex, no line hash.
    pub(super) seventv_overlay: Option<super::super::seventv_paint::PaintedLineOverlay>,
    /// Name paints resolved at paint time: animated paints (current decoded
    /// frame) and paints with an image layer that was not decoded when the row
    /// was prepared. Fully resolved static paints keep the prepared overlay
    /// fast path above.
    pub(super) live_seventv_paint: Option<Arc<super::super::seventv_paint::SevenTvLinePaint>>,
    /// Name-paint composites can extend beyond the normal line bounds through
    /// shadows or strokes. The viewport culler keeps these rows paintable until
    /// their exact overflow bounds are represented in layout metadata.
    pub(super) has_seventv_paint: bool,
}

#[derive(Clone)]
pub(super) struct PreparedImagePaintCommand {
    pub(super) command_index: usize,
    pub(super) static_render_image: Option<Arc<RenderImage>>,
}

impl ViewportPaintRow {
    #[inline]
    pub(super) fn may_paint_outside_row_bounds(&self) -> bool {
        self.text_commands
            .iter()
            .any(|command| command.has_seventv_paint)
            || self
                .layout
                .image_paint_commands
                .iter()
                .any(|command| command.y < 0.0 || command.y + command.height > self.layout.height)
    }
}

#[inline]
pub(super) fn can_paint_visible_row_in_viewport_canvas(
    element_render_plan: &super::element_render::ElementRenderPlan,
    is_deleted: bool,
) -> bool {
    element_render_plan.is_empty() && !is_deleted
}

#[inline]
pub(super) fn prepare_viewport_paint_row(
    row: &VisibleRenderRow,
    selection_ranges: SelectionRanges,
    search_focused: bool,
    window: &mut Window,
) -> ViewportPaintRow {
    let image_commands = prepare_image_paint_commands(&row.layout);
    ViewportPaintRow {
        layout: Arc::clone(&row.layout),
        message_index: row.message_index,
        selection_ranges,
        viewport_y: row.viewport_y,
        search_focused,
        is_alternate_row: row.is_alternate_row,
        text_commands: prepare_text_paint_commands(&row.layout, window),
        image_commands,
    }
}

#[inline]
pub(super) fn reuse_prepared_viewport_paint_row(
    cached_rows: &[ViewportPaintRow],
    row: &VisibleRenderRow,
) -> Option<ViewportPaintRow> {
    let index = cached_rows
        .binary_search_by_key(&row.message_index, |cached| cached.message_index)
        .ok()?;
    let cached = &cached_rows[index];
    if !Arc::ptr_eq(&cached.layout, &row.layout) {
        return None;
    }

    let mut reused = cached.clone();
    reused.viewport_y = row.viewport_y;
    reused.is_alternate_row = row.is_alternate_row;
    Some(reused)
}

#[inline]
pub(super) fn paint_message_layout_direct(
    layout: &CanvasMessageLayout,
    selection_ranges: &[std::ops::Range<usize>],
    origin: Point<Pixels>,
    container_width: f32,
    search_focused: bool,
    alternate_row_bg: Option<Hsla>,
    animation_slots: &mut super::image_paint::AnimationSlotSink<'_>,
    window: &mut Window,
    cx: &mut App,
) {
    if let Some(bg) = alternate_row_bg {
        let bg_bounds = Bounds::new(origin, size(px(container_width), px(layout.height)));
        window.paint_quad(fill(bg_bounds, bg));
    }

    if let Some(bg) = layout
        .highlight_type
        .as_ref()
        .and_then(|h| h.background_color())
    {
        let bg_bounds = Bounds::new(origin, size(px(container_width), px(layout.height)));
        window.paint_quad(fill(bg_bounds, bg));
    }

    let low_trust = layout.low_trust.colors();
    if let Some((bg, _)) = low_trust {
        let bg_bounds = Bounds::new(origin, size(px(container_width), px(layout.height)));
        window.paint_quad(fill(bg_bounds, bg));
    }

    if search_focused {
        let bg_bounds = Bounds::new(origin, size(px(container_width), px(layout.height)));
        window.paint_quad(fill(bg_bounds, colors::accent_transparent(0.18)));
        let border_bounds = Bounds::new(origin, size(px(2.0), px(layout.height)));
        window.paint_quad(fill(border_bounds, colors::accent()));
    }

    let border = layout
        .highlight_type
        .as_ref()
        .and_then(|h| h.border_color())
        .or(low_trust.map(|(_, border)| border));

    let mention_bg = colors::mention_bg();

    for layer in message_paint_layers() {
        match layer {
            MessagePaintLayer::Border => {
                if let Some(border_color) = border {
                    let border_bounds = Bounds::new(origin, size(px(2.0), px(layout.height)));
                    window.paint_quad(fill(border_bounds, border_color));
                }
            }
            MessagePaintLayer::MentionHighlights => {
                paint_mention_highlights_direct(layout, origin, mention_bg, window);
            }
            MessagePaintLayer::SelectionHighlights => {
                paint_selection_highlights_direct(selection_ranges, layout, None, origin, window);
            }
            MessagePaintLayer::Text => {
                paint_text_direct(
                    &layout.text_paint_commands,
                    layout,
                    origin,
                    animation_slots,
                    window,
                    cx,
                );
            }
            MessagePaintLayer::Images => {
                paint_images_direct(layout, origin, window, animation_slots);
            }
            MessagePaintLayer::ImageSelectionHighlights => {
                paint_image_selection_highlights_direct(selection_ranges, layout, origin, window);
            }
        }
    }
}

#[inline]
pub(super) fn paint_message_layout_prepared_direct(
    row: &ViewportPaintRow,
    origin: Point<Pixels>,
    container_width: f32,
    line_height: f32,
    mention_bg: Hsla,
    alternate_bg: Option<Hsla>,
    animation_slots: &mut super::image_paint::AnimationSlotSink<'_>,
    window: &mut Window,
    cx: &mut App,
) {
    if row.is_alternate_row
        && let Some(bg) = alternate_bg
    {
        let bg_bounds = Bounds::new(origin, size(px(container_width), px(row.layout.height)));
        window.paint_quad(fill(bg_bounds, bg));
    }

    if let Some(bg) = row
        .layout
        .highlight_type
        .as_ref()
        .and_then(|h| h.background_color())
    {
        let bg_bounds = Bounds::new(origin, size(px(container_width), px(row.layout.height)));
        window.paint_quad(fill(bg_bounds, bg));
    }

    let low_trust = row.layout.low_trust.colors();
    if let Some((bg, _)) = low_trust {
        let bg_bounds = Bounds::new(origin, size(px(container_width), px(row.layout.height)));
        window.paint_quad(fill(bg_bounds, bg));
    }

    if row.search_focused {
        let bg_bounds = Bounds::new(origin, size(px(container_width), px(row.layout.height)));
        window.paint_quad(fill(bg_bounds, colors::accent_transparent(0.18)));
        let border_bounds = Bounds::new(origin, size(px(2.0), px(row.layout.height)));
        window.paint_quad(fill(border_bounds, colors::accent()));
    }

    let border = row
        .layout
        .highlight_type
        .as_ref()
        .and_then(|h| h.border_color())
        .or(low_trust.map(|(_, border)| border));

    for layer in message_paint_layers() {
        match layer {
            MessagePaintLayer::Border => {
                if let Some(border_color) = border {
                    let border_bounds = Bounds::new(origin, size(px(2.0), px(row.layout.height)));
                    window.paint_quad(fill(border_bounds, border_color));
                }
            }
            MessagePaintLayer::MentionHighlights => {
                paint_mention_highlights_direct(&row.layout, origin, mention_bg, window);
            }
            MessagePaintLayer::SelectionHighlights => {
                paint_selection_highlights_direct(
                    &row.selection_ranges,
                    &row.layout,
                    Some(&row.text_commands),
                    origin,
                    window,
                );
            }
            MessagePaintLayer::Text => {
                paint_prepared_text_direct(
                    &row.text_commands,
                    &row.layout,
                    origin,
                    line_height,
                    animation_slots,
                    window,
                    cx,
                );
            }
            MessagePaintLayer::Images => {
                paint_prepared_images_direct(
                    &row.layout,
                    &row.image_commands,
                    origin,
                    window,
                    animation_slots,
                );
            }
            MessagePaintLayer::ImageSelectionHighlights => {
                paint_image_selection_highlights_direct(
                    &row.selection_ranges,
                    &row.layout,
                    origin,
                    window,
                );
            }
        }
    }
}

/// Paint mention highlight background rectangles (for @mentions of current user).
///
/// These are computed during layout building and stored in `layout.mention_highlights`.
#[inline]
fn paint_mention_highlights_direct(
    layout: &CanvasMessageLayout,
    origin: Point<Pixels>,
    bg: Hsla,
    window: &mut Window,
) {
    if layout.mention_highlights.is_empty() {
        return;
    }

    for (x, y, width, height) in layout.mention_highlights.iter().copied() {
        if width <= 0.0 || height <= 0.0 {
            continue;
        }
        let bounds = Bounds::new(origin + point(px(x), px(y)), size(px(width), px(height)));
        window.paint_quad(fill(bounds, bg));
    }
}

/// Paint selection highlights for a message.
///
/// Selection ranges are character offsets into the layout's source text (Unicode scalar values).
/// We paint per-shaped-text segment to correctly handle wrapping, inline emotes, and mixed styling.
#[inline]
fn paint_selection_highlights_direct(
    ranges: &[std::ops::Range<usize>],
    layout: &CanvasMessageLayout,
    prepared_text: Option<&[PreparedTextPaintCommand]>,
    origin: Point<Pixels>,
    window: &mut Window,
) {
    if ranges.is_empty() {
        return;
    }
    let text_h = text_height();

    for segment in &layout.segments {
        let LayoutSegment::Text {
            line_idx,
            x,
            y,
            height,
            ..
        } = segment
        else {
            continue;
        };

        let Some(line_data) = layout.shaped_lines.get(*line_idx) else {
            continue;
        };
        if line_data.source_len_chars == 0 {
            continue;
        }

        let line_height = *height;
        if line_height <= 0.0 {
            continue;
        }
        let line_top = *y - (*height - text_h).max(0.0);

        let seg_start = line_data.source_start_char;
        let seg_end = seg_start.saturating_add(line_data.source_len_chars);

        for range in ranges {
            let s = range.start.max(seg_start);
            let e = range.end.min(seg_end);
            if e <= s {
                continue;
            }

            let local_s = s - seg_start;
            let local_e = e - seg_start;

            let shaped_line = prepared_text
                .and_then(|commands| {
                    commands
                        .iter()
                        .find(|command| command.line_idx == *line_idx)
                        .map(|command| Rc::clone(&command.shaped_line))
                })
                .unwrap_or_else(|| {
                    super::text_paint::cached_shape_line(
                        &line_data.text,
                        scaled_font_size(),
                        &line_data.gpui_runs,
                        line_data.gpui_runs_hash,
                        window,
                    )
                });
            let byte_offset = |character_offset| {
                line_data
                    .text
                    .char_indices()
                    .nth(character_offset)
                    .map(|(byte_offset, _)| byte_offset)
                    .unwrap_or(line_data.text.len())
            };

            for span in
                shaped_line.visual_spans_for_range(byte_offset(local_s)..byte_offset(local_e))
            {
                let bounds = Bounds::new(
                    origin + point(px(*x) + span.start, px(line_top)),
                    size(span.end - span.start, px(line_height)),
                );
                window.paint_quad(fill(bounds, SELECTION_COLOR));
            }
        }
    }
}

/// Paint selection tint over emote/image segments whose source-text char range
/// overlaps any selection range.
///
/// Runs *after* `Images` in `message_paint_layers()` (see `MessagePaintLayer`)
/// -- unlike text highlighting, which paints before `Text` and relies on the
/// glyphs drawing over an opaque band, emote images are themselves opaque and
/// would hide a tint painted earlier. Unlike text, there's no partial-character
/// clip within a segment: any overlap tints the emote's *full* bounding box
/// (all-or-nothing): selecting part of an emote selects the whole emote,
/// rather than partially highlighting one image.
#[inline]
fn paint_image_selection_highlights_direct(
    ranges: &[std::ops::Range<usize>],
    layout: &CanvasMessageLayout,
    origin: Point<Pixels>,
    window: &mut Window,
) {
    if ranges.is_empty() {
        return;
    }

    for segment in &layout.segments {
        let (x, y, width, height, seg_start, seg_end) = match segment {
            LayoutSegment::LayeredImage {
                x,
                y,
                width,
                height,
                source_start_char,
                source_len_chars,
                ..
            } => (
                *x,
                *y,
                *width,
                *height,
                *source_start_char,
                source_start_char.saturating_add(*source_len_chars),
            ),
            LayoutSegment::Image {
                x,
                y,
                width,
                height,
                source_range: Some((start, len)),
                ..
            } => (*x, *y, *width, *height, *start, start.saturating_add(*len)),
            _ => continue,
        };

        if width <= 0.0 || height <= 0.0 || seg_end <= seg_start {
            continue;
        }

        let overlaps = ranges
            .iter()
            .any(|range| range.start.max(seg_start) < range.end.min(seg_end));
        if !overlaps {
            continue;
        }

        let bounds = Bounds::new(origin + point(px(x), px(y)), size(px(width), px(height)));
        window.paint_quad(fill(bounds, SELECTION_COLOR));
    }
}
