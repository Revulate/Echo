//! Custom GPUI element for painting prepared visible chat rows.
//!
//! This keeps viewport painting in GPUI's request-layout/prepaint/paint model and
//! applies an explicit content mask so prepared rows cannot bleed outside the chat viewport.

use super::direct_paint::{ViewportPaintRow, paint_message_layout_prepared_direct};
use super::image_paint::AnimationSlotSink;
use super::*;

pub(super) fn render_visible_rows_viewport_element(
    rows: Arc<Vec<ViewportPaintRow>>,
    container_width: f32,
    scroll_delta_y: f32,
    animation_slots: Rc<RefCell<PaintedAnimationSlots>>,
    slot_owner: u64,
) -> impl IntoElement {
    let alternate_bg = crate::backend::settings::get_alternate_message_backgrounds()
        .then(colors::bg_alternate_row);
    ChatViewportElement::new(
        rows,
        container_width,
        scroll_delta_y,
        text_height(),
        colors::mention_bg(),
        alternate_bg,
        animation_slots,
        slot_owner,
    )
    .absolute()
    .inset_0()
    .size_full()
}

struct ChatViewportElement {
    rows: Arc<Vec<ViewportPaintRow>>,
    container_width: f32,
    scroll_delta_y: f32,
    line_height: f32,
    mention_bg: Hsla,
    alternate_bg: Option<Hsla>,
    animation_slots: Rc<RefCell<PaintedAnimationSlots>>,
    slot_owner: u64,
    style: StyleRefinement,
}

struct ChatViewportPrepaintState {
    bounds: Bounds<Pixels>,
}

impl ChatViewportElement {
    fn new(
        rows: Arc<Vec<ViewportPaintRow>>,
        container_width: f32,
        scroll_delta_y: f32,
        line_height: f32,
        mention_bg: Hsla,
        alternate_bg: Option<Hsla>,
        animation_slots: Rc<RefCell<PaintedAnimationSlots>>,
        slot_owner: u64,
    ) -> Self {
        Self {
            rows,
            container_width,
            scroll_delta_y,
            line_height,
            mention_bg,
            alternate_bg,
            animation_slots,
            slot_owner,
            style: StyleRefinement::default(),
        }
    }
}

impl IntoElement for ChatViewportElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for ChatViewportElement {
    type RequestLayoutState = Style;
    type PrepaintState = ChatViewportPrepaintState;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let mut style = Style::default();
        style.refine(&self.style);
        let layout_id = window.request_layout(style.clone(), [], cx);
        (layout_id, style)
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        _window: &mut Window,
        _cx: &mut App,
    ) -> Self::PrepaintState {
        ChatViewportPrepaintState { bounds }
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        style: &mut Self::RequestLayoutState,
        prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let paint_bounds = prepaint.bounds;
        let paint_emit_start = RenderTimer::start(crate::fps_profiler::is_enabled());
        let mut animation_slots = self.animation_slots.borrow_mut();
        let mut sink = AnimationSlotSink {
            slots: &mut animation_slots,
            owner: self.slot_owner,
            row: 0,
        };
        style.paint(bounds, window, cx, |window, cx| {
            window.with_content_mask(
                Some(ContentMask {
                    bounds: paint_bounds,
                }),
                |window| {
                    let base = paint_bounds.origin;
                    let viewport_height: f32 = paint_bounds.size.height.into();
                    for row in self.rows.iter() {
                        let row_y = row.viewport_y + self.scroll_delta_y;
                        if !row_intersects_viewport(
                            row_y,
                            row.layout.height,
                            viewport_height,
                            false,
                        ) && !row.may_paint_outside_row_bounds()
                        {
                            continue;
                        }
                        let origin = base + point(px(0.0), px(row_y));
                        sink.row = row.message_index;
                        paint_message_layout_prepared_direct(
                            row,
                            origin,
                            self.container_width,
                            self.line_height,
                            self.mention_bg,
                            self.alternate_bg,
                            &mut sink,
                            window,
                            cx,
                        );
                    }
                },
            );
        });
        let paint_emit_elapsed = paint_emit_start.elapsed();
        if crate::fps_profiler::is_enabled() {
            crate::fps_profiler::record_canvas_paint_nanos(paint_emit_elapsed.as_nanos() as u64);
            crate::fps_profiler::record_phase("paint_emit", paint_emit_elapsed);
        }
    }
}

#[inline]
fn row_intersects_viewport(
    row_y: f32,
    row_height: f32,
    viewport_height: f32,
    may_paint_outside_row_bounds: bool,
) -> bool {
    const RASTER_MARGIN: f32 = 2.0;
    may_paint_outside_row_bounds
        || (row_y < viewport_height + RASTER_MARGIN && row_y + row_height > -RASTER_MARGIN)
}

#[cfg(test)]
mod tests {
    use super::row_intersects_viewport;
    use core::prelude::v1::test;

    #[test]
    fn culls_only_fully_clipped_rows_with_bounded_paint() {
        assert!(!row_intersects_viewport(-20.0, 10.0, 100.0, false));
        assert!(!row_intersects_viewport(102.0, 10.0, 100.0, false));
        assert!(row_intersects_viewport(101.0, 10.0, 100.0, false));
        assert!(row_intersects_viewport(-11.0, 10.0, 100.0, false));
        assert!(row_intersects_viewport(-1.0, 10.0, 100.0, false));
        assert!(row_intersects_viewport(99.0, 10.0, 100.0, false));
    }

    #[test]
    fn retains_rows_with_unbounded_name_paint_overflow() {
        assert!(row_intersects_viewport(-20.0, 10.0, 100.0, true));
        assert!(row_intersects_viewport(110.0, 10.0, 100.0, true));
    }
}

impl Styled for ChatViewportElement {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}
