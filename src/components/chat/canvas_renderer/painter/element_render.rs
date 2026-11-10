//! Element fallback rendering for message segments that cannot be direct-painted.

use super::element_images::{image_shows_still_frame, render_image_segment, render_layered_image};
use super::element_widgets::{
    render_embed_preview, render_inline_box, render_reply_indicator, render_upload_preview,
};
use super::*;
use gpui::prelude::FluentBuilder;
use smallvec::SmallVec;

/// Per-frame fallback decision for one message layout.
///
/// Animated image readiness can change between frames, so this deliberately
/// stays out of retained layout and viewport-paint caches. Keeping the segment
/// indices for the current render pass lets the direct-paint eligibility check
/// and fallback element builder share one classification scan.
#[derive(Default)]
pub(super) struct ElementRenderPlan {
    segment_indices: SmallVec<[usize; 2]>,
    /// `FALLBACK_*` bits for why those segments need elements.
    reasons: u8,
}

pub(super) const FALLBACK_DELETED: u8 = 1 << 0;
pub(super) const FALLBACK_SVG: u8 = 1 << 1;
pub(super) const FALLBACK_ANIMATED_NOT_LOADED: u8 = 1 << 2;
pub(super) const FALLBACK_ANIMATED_LOADING: u8 = 1 << 3;
pub(super) const FALLBACK_INLINE_BOX: u8 = 1 << 4;
pub(super) const FALLBACK_REPLY_INDICATOR: u8 = 1 << 5;
pub(super) const FALLBACK_EMBED_PREVIEW: u8 = 1 << 6;
pub(super) const FALLBACK_UPLOAD_PREVIEW: u8 = 1 << 7;

impl ElementRenderPlan {
    #[inline]
    pub(super) fn for_layout(layout: &CanvasMessageLayout) -> Self {
        if !layout.has_element_segments {
            return Self::default();
        }

        let mut segment_indices = SmallVec::new();
        let mut reasons = 0;
        for (index, segment) in layout.segments.iter().enumerate() {
            let segment_reasons = segment_element_fallback_reasons(segment);
            if segment_reasons != 0 {
                segment_indices.push(index);
                reasons |= segment_reasons;
            }
        }
        Self {
            segment_indices,
            reasons,
        }
    }

    #[inline]
    pub(super) fn is_empty(&self) -> bool {
        self.segment_indices.is_empty()
    }

    /// Whether an element segment of this plan shows an animated image that is
    /// still loading. The row canvas paints every other animated image as a
    /// patchable slot.
    #[inline]
    pub(super) fn has_loading_animation(&self) -> bool {
        self.reasons & (FALLBACK_ANIMATED_NOT_LOADED | FALLBACK_ANIMATED_LOADING) != 0
    }

    /// `FALLBACK_*` bits keeping a row of this plan out of the direct canvas.
    #[inline]
    pub(super) fn fallback_reasons(&self, is_deleted: bool) -> u8 {
        if is_deleted {
            self.reasons | FALLBACK_DELETED
        } else {
            self.reasons
        }
    }
}

/// How much of a deleted row shows through: removed message bodies fade to
/// half strength, as the 7TV extension does.
const DELETED_ROW_OPACITY: f32 = 0.5;

/// Frame-wide inputs shared by every row rendered through the element fallback.
pub(super) struct FallbackRowFrame<'a> {
    pub(super) selection: &'a SelectionState,
    pub(super) container_width: f32,
    /// Background of alternate (zebra) rows, when that setting is on.
    pub(super) alternate_bg: Option<Hsla>,
    pub(super) animation_slots: &'a Rc<RefCell<PaintedAnimationSlots>>,
    pub(super) slot_owner: u64,
}

/// Render one visible row as GPUI elements at its viewport position, for rows
/// the direct canvas cannot paint (see [`ElementRenderPlan`]).
#[inline]
pub(super) fn render_message_at_position(
    frame: &FallbackRowFrame<'_>,
    row: &VisibleRenderRow,
    element_render_plan: &ElementRenderPlan,
    search_focused: bool,
) -> impl IntoElement + use<> {
    let layout = Arc::clone(&row.layout);
    let selection = frame.selection;
    let message_index = row.message_index;
    let container_width = frame.container_width;
    let y_position = row.viewport_y;
    let alternate_row_bg = if row.is_alternate_row {
        frame.alternate_bg
    } else {
        None
    };
    let is_deleted = row.is_deleted;
    let animation_slots = Rc::clone(frame.animation_slots);
    let slot_owner = frame.slot_owner;
    let selection_ranges =
        selection.get_ranges_for_message(message_index, layout.source_char_len());

    let mut msg_div = div()
        .id(layout.message_id_shared.clone())
        .absolute()
        .left(px(0.0))
        .top(px(y_position))
        .w(px(container_width))
        .h(px(layout.height))
        .overflow_hidden()
        .when(is_deleted, |element| element.opacity(DELETED_ROW_OPACITY));

    let layout_for_canvas = Arc::clone(&layout);
    let selection_ranges_for_canvas = selection_ranges.clone();

    msg_div = msg_div.child(
        canvas(
            move |_bounds, _window, _cx| {},
            move |bounds, (), window, cx| {
                let origin = bounds.origin;
                let mut animation_slots = animation_slots.borrow_mut();
                let mut sink = super::image_paint::AnimationSlotSink {
                    slots: &mut animation_slots,
                    owner: slot_owner,
                    row: message_index,
                };
                super::direct_paint::paint_message_layout_direct(
                    &layout_for_canvas,
                    &selection_ranges_for_canvas,
                    origin,
                    container_width,
                    search_focused,
                    alternate_row_bg,
                    &mut sink,
                    window,
                    cx,
                );
            },
        )
        .absolute()
        .inset_0()
        .size_full(),
    );

    for &segment_index in &element_render_plan.segment_indices {
        if let Some(segment) = layout.segments.get(segment_index) {
            msg_div = msg_div.child(render_segment(segment, &layout));
        }
    }

    msg_div
}

/// `FALLBACK_*` bits for why `segment` needs a GPUI element; zero when the
/// direct canvas can paint it.
#[inline]
fn segment_element_fallback_reasons(segment: &LayoutSegment) -> u8 {
    match segment {
        LayoutSegment::Text { .. } => 0,
        LayoutSegment::Image {
            url,
            is_animated,
            overlays,
            ..
        } => overlays.iter().fold(
            image_element_fallback_reasons(url.as_ref(), *is_animated),
            |reasons, overlay| {
                reasons | image_element_fallback_reasons(overlay.url.as_ref(), overlay.is_animated)
            },
        ),
        LayoutSegment::LayeredImage { layers, .. } => layers.iter().fold(0, |reasons, layer| {
            reasons | image_element_fallback_reasons(layer.url.as_ref(), layer.is_animated)
        }),
        LayoutSegment::InlineBox { .. } => FALLBACK_INLINE_BOX,
        LayoutSegment::EmbedPreview { .. } => FALLBACK_EMBED_PREVIEW,
        LayoutSegment::UploadPreview { .. } => FALLBACK_UPLOAD_PREVIEW,
        LayoutSegment::ReplyIndicator { .. } => FALLBACK_REPLY_INDICATOR,
    }
}

#[inline]
fn image_element_fallback_reasons(url: &str, is_animated: bool) -> u8 {
    if is_svg_image_url(url) {
        return FALLBACK_SVG;
    }
    if !is_animated {
        return 0;
    }
    match image_cache::get_animated_cache_state(url) {
        image_cache::AnimatedCacheState::Ready | image_cache::AnimatedCacheState::Failed => 0,
        image_cache::AnimatedCacheState::NotLoaded => FALLBACK_ANIMATED_NOT_LOADED,
        image_cache::AnimatedCacheState::Loading => FALLBACK_ANIMATED_LOADING,
    }
}

/// Render a single layout segment
#[inline]
fn render_segment(segment: &LayoutSegment, layout: &CanvasMessageLayout) -> AnyElement {
    match segment {
        LayoutSegment::Text {
            line_idx,
            x,
            y,
            width,
            height,
        } => {
            let line_data = layout.shaped_lines.get(*line_idx);

            if let Some(data) = line_data {
                render_text_segment(data, *x, *y, *width, *height)
            } else {
                div().into_any_element()
            }
        }
        LayoutSegment::Image {
            url,
            x,
            y,
            width,
            height,
            is_animated,
            overlays,
            image_type,
            ..
        } => render_image_segment(
            url,
            *x,
            *y,
            *width,
            *height,
            *is_animated,
            image_shows_still_frame(image_type),
            overlays,
        ),
        LayoutSegment::LayeredImage {
            layers,
            x,
            y,
            width,
            height,
            ..
        } => render_layered_image(layers, *x, *y, *width, *height),
        LayoutSegment::InlineBox {
            text,
            x,
            y,
            width,
            height,
            bg_color,
            text_color,
            font_size,
        } => render_inline_box(
            text,
            *x,
            *y,
            *width,
            *height,
            *bg_color,
            *text_color,
            *font_size,
        ),
        LayoutSegment::EmbedPreview {
            url,
            platform,
            thumbnail_url,
            title,
            description,
            subtitle,
            duration,
            x,
            y,
            width,
            height,
            geometry,
            is_expanded,
            is_loading,
            ..
        } => render_embed_preview(
            url,
            platform,
            thumbnail_url.as_ref(),
            title.as_ref(),
            description.as_ref(),
            subtitle.as_ref(),
            duration.as_ref(),
            *x,
            *y,
            *width,
            *height,
            geometry,
            *is_expanded,
            *is_loading,
        ),
        LayoutSegment::UploadPreview {
            url,
            thumbnail_url,
            thumbnail_width,
            x,
            y,
            width,
            height,
        } => render_upload_preview(
            url,
            thumbnail_url.as_ref(),
            *thumbnail_width,
            *x,
            *y,
            *width,
            *height,
        ),
        LayoutSegment::ReplyIndicator {
            connector_x,
            connector_y,
            connector_height,
            parent_display_name,
            parent_color,
            preview_text,
            text_x,
            width,
        } => render_reply_indicator(
            *connector_x,
            *connector_y,
            *connector_height,
            parent_display_name,
            *parent_color,
            preview_text,
            *text_x,
            *width,
        ),
    }
}

/// Render a text segment
#[inline]
fn render_text_segment(
    data: &ShapedLineData,
    x: f32,
    y: f32,
    _width: f32,
    _height: f32,
) -> AnyElement {
    let runs = &data.gpui_runs;
    let first_run = runs.first();
    let default_color = first_run.map(|r| r.color).unwrap_or(colors::text_primary());
    let is_bold = first_run
        .map(|r| r.font.weight == FontWeight::SEMIBOLD)
        .unwrap_or(false);
    let is_underline = first_run.is_some_and(|r| r.underline.is_some());

    let has_multiple_colors = runs.len() > 1
        && runs.windows(2).any(|w| {
            (w[0].color.h - w[1].color.h).abs() > 0.01
                || (w[0].color.s - w[1].color.s).abs() > 0.01
                || (w[0].color.l - w[1].color.l).abs() > 0.01
        });

    let mut text_div = div().absolute().left(px(x)).top(px(y)).text_sm();

    if is_bold {
        text_div = text_div.font_weight(FontWeight::SEMIBOLD);
    }

    if is_underline {
        text_div = text_div.underline();
    }

    if has_multiple_colors {
        let mut highlights: Vec<(std::ops::Range<usize>, HighlightStyle)> = Vec::new();
        let mut byte_offset = 0usize;
        let text_bytes = data.text.len();

        for run in runs {
            let end = (byte_offset + run.len).min(text_bytes);
            if byte_offset >= text_bytes {
                break;
            }
            let range = byte_offset..end;
            highlights.push((
                range,
                HighlightStyle {
                    color: Some(run.color),
                    font_weight: if run.font.weight == FontWeight::SEMIBOLD {
                        Some(FontWeight::SEMIBOLD)
                    } else {
                        None
                    },
                    ..Default::default()
                },
            ));
            byte_offset = end;
        }

        text_div = text_div.child(StyledText::new(data.text.clone()).with_highlights(highlights));
    } else {
        text_div = text_div.text_color(default_color).child(data.text.clone());
    }

    text_div.into_any_element()
}
