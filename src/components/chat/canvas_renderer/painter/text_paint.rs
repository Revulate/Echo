//! Direct text paint preparation, shaping cache, and paint execution.

use super::direct_paint::PreparedTextPaintCommand;
use super::*;

use crate::components::chat::canvas_renderer::layout::hash_gpui_text_runs;
use crate::components::chat::canvas_renderer::seventv_paint;
use lru::LruCache;

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct TextShapeCacheKey {
    text: SharedString,
    font_size_bits: u32,
    runs_hash: u64,
}

impl TextShapeCacheKey {
    #[inline]
    fn new(text: &SharedString, font_size: f32, runs_hash: u64) -> Self {
        Self {
            text: text.clone(),
            font_size_bits: font_size.to_bits(),
            runs_hash,
        }
    }
}

struct TextShapeLineCache {
    inner: LruCache<TextShapeCacheKey, Rc<ShapedLine>>,
}

impl Default for TextShapeLineCache {
    fn default() -> Self {
        Self {
            inner: LruCache::new(
                NonZeroUsize::new(TEXT_SHAPE_CACHE_CAPACITY)
                    .expect("TEXT_SHAPE_CACHE_CAPACITY must be non-zero"),
            ),
        }
    }
}

impl TextShapeLineCache {
    #[inline]
    fn get(&mut self, key: &TextShapeCacheKey) -> Option<Rc<ShapedLine>> {
        self.inner.get(key).map(Rc::clone)
    }

    #[inline]
    fn insert(&mut self, key: TextShapeCacheKey, shaped_line: Rc<ShapedLine>) {
        self.inner.put(key, shaped_line);
    }
}

thread_local! {
    static TEXT_SHAPE_LINE_CACHE: RefCell<TextShapeLineCache> =
        RefCell::new(TextShapeLineCache::default());
}

pub(super) fn prepare_text_paint_commands(
    layout: &CanvasMessageLayout,
    window: &mut Window,
) -> SmallVec<[PreparedTextPaintCommand; 4]> {
    let mut prepared = SmallVec::with_capacity(layout.text_paint_commands.len());
    let font_size = scaled_font_size();

    for cmd in &layout.text_paint_commands {
        let Some(data) = layout.shaped_lines.get(cmd.line_idx) else {
            continue;
        };
        if data.text.is_empty() {
            continue;
        }

        let shaped_line = cached_shape_line(
            &data.text,
            font_size,
            &data.gpui_runs,
            data.gpui_runs_hash,
            window,
        );

        let live_seventv_paint = data
            .seventv_paint
            .as_ref()
            .filter(|paint| needs_live_overlay(paint))
            .cloned();
        let seventv_overlay = if live_seventv_paint.is_none() {
            resolve_prepared_overlay(data, &shaped_line, window)
        } else {
            None
        };

        prepared.push(PreparedTextPaintCommand {
            line_idx: cmd.line_idx,
            x: cmd.x,
            y: cmd.y,
            shaped_line,
            seventv_overlay,
            live_seventv_paint,
            has_seventv_paint: data.seventv_paint.is_some(),
        });
    }

    prepared
}

/// Paint all text segments directly using shaped_line.paint().
#[inline]
pub(super) fn paint_text_direct(
    commands: &[TextPaintCommand],
    layout: &CanvasMessageLayout,
    origin: Point<Pixels>,
    animation_slots: &mut super::image_paint::AnimationSlotSink<'_>,
    window: &mut Window,
    cx: &mut App,
) {
    let current_line_height = text_height();
    let font_size = scaled_font_size();

    for cmd in commands {
        if let Some(line_data) = layout.shaped_lines.get(cmd.line_idx) {
            paint_single_text_direct(
                line_data,
                cmd.x,
                cmd.y,
                font_size,
                current_line_height,
                origin,
                (animation_slots, layout, cmd.line_idx),
                window,
                cx,
            );
        }
    }
}

#[inline]
pub(super) fn cached_shape_line(
    text: &SharedString,
    font_size: f32,
    runs: &[TextRun],
    runs_hash: u64,
    window: &mut Window,
) -> Rc<ShapedLine> {
    let key = TextShapeCacheKey::new(text, font_size, runs_hash);
    TEXT_SHAPE_LINE_CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        if let Some(shaped_line) = cache.get(&key) {
            return shaped_line;
        }

        let shaped_line = Rc::new(window.text_system().shape_line(
            text.clone(),
            px(font_size),
            runs,
            None,
        ));
        cache.insert(key, Rc::clone(&shaped_line));
        shaped_line
    })
}

/// Paint a single text segment directly to GPU.
#[inline]
fn paint_single_text_direct(
    data: &ShapedLineData,
    x: f32,
    y: f32,
    font_size: f32,
    current_line_height: f32,
    origin: Point<Pixels>,
    (animation_slots, layout, line_index): (
        &mut super::image_paint::AnimationSlotSink<'_>,
        &CanvasMessageLayout,
        usize,
    ),
    window: &mut Window,
    cx: &mut App,
) {
    if data.text.is_empty() {
        return;
    }

    let position = origin + point(px(x), px(y));
    let shaped_line = cached_shape_line(
        &data.text,
        font_size,
        &data.gpui_runs,
        data.gpui_runs_hash,
        window,
    );

    let overlay = data.seventv_paint.as_deref().map(|spec| {
        seventv_paint::overlay_for_line(spec, &shaped_line, font_size, current_line_height, window)
    });
    match overlay {
        Some(None) => {
            if data
                .seventv_paint
                .as_deref()
                .is_some_and(|paint| paint.is_animated())
            {
                animation_slots.slots.unslotted |= UNSLOTTED_NAME_PAINT;
            }
            paint_line_with_unpainted_name(
                data,
                position,
                font_size,
                current_line_height,
                window,
                cx,
            )
        }
        overlay => {
            let _ = shaped_line.paint(
                position,
                px(current_line_height),
                TextAlign::Left,
                None,
                window,
                cx,
            );
            if let (Some(Some(overlay)), Some(spec)) = (overlay, data.seventv_paint.as_ref()) {
                animation_slots.paint_name_overlay(
                    &overlay,
                    spec,
                    &shaped_line,
                    current_line_height,
                    layout,
                    line_index,
                    position,
                    window,
                );
            }
        }
    }
    crate::startup_metrics::record("first_text_paint");
}

/// Paint a name-painted line with its painted run in the username's base
/// color (CSS `currentColor`, the Twitch color).
///
/// Layout draws that run transparent because the composite overlay replaces
/// it, so a composite that cannot be built (for example a failed glyph mask
/// rasterization) would otherwise leave the name invisible. SevenTV shows
/// `currentColor` wherever its paint does not cover the glyphs.
fn paint_line_with_unpainted_name(
    data: &ShapedLineData,
    position: Point<Pixels>,
    font_size: f32,
    line_height: f32,
    window: &mut Window,
    cx: &mut App,
) {
    let Some(spec) = data.seventv_paint.as_deref() else {
        return;
    };
    let base = spec.base_color;
    let base_color = Hsla::from(Rgba {
        r: base.r as f32 / 255.0,
        g: base.g as f32 / 255.0,
        b: base.b as f32 / 255.0,
        a: base.a as f32 / 255.0,
    });
    let mut runs = data.gpui_runs.clone();
    let mut run_start = 0;
    for run in &mut runs {
        let run_end = run_start + run.len;
        if run_start < spec.byte_range.end && spec.byte_range.start < run_end {
            run.color = base_color;
        }
        run_start = run_end;
    }
    let shaped_line = cached_shape_line(
        &data.text,
        font_size,
        &runs,
        hash_gpui_text_runs(&runs),
        window,
    );
    let _ = shaped_line.paint(position, px(line_height), TextAlign::Left, None, window, cx);
}

/// Whether a name paint must resolve its overlay at paint time.
///
/// Prepared rows outlive the frame they were prepared in (snapshot and per-row
/// reuse keyed by the layout `Arc`), and the image-load waker only repaints.
/// Animated paints change frames, and a static image layer that is not decoded
/// yet would freeze the `currentColor`-only composite into the prepared row, so
/// both go through the live lookup that picks up the decoded texture.
#[inline]
fn needs_live_overlay(paint: &seventv_paint::SevenTvLinePaint) -> bool {
    paint
        .image_layers()
        .any(|(url, animated)| animated || !image_cache::has_cached_render_image(url))
}

/// Resolve the exact 7TV name-paint overlay for a prepared line once at prepare
/// time, so the per-frame paint path is a plain `Option` deref with no global
/// `COMPOSITE_CACHE` mutex lock and no per-frame full-line text hash.
#[inline]
fn resolve_prepared_overlay(
    data: &ShapedLineData,
    shaped_line: &ShapedLine,
    window: &mut Window,
) -> Option<seventv_paint::PaintedLineOverlay> {
    let spec = data.seventv_paint.as_deref()?;
    seventv_paint::overlay_for_line(
        spec,
        shaped_line,
        f32::from(shaped_line.font_size),
        text_height(),
        window,
    )
}

#[inline]
pub(super) fn paint_prepared_text_direct(
    commands: &[PreparedTextPaintCommand],
    layout: &CanvasMessageLayout,
    origin: Point<Pixels>,
    line_height: f32,
    animation_slots: &mut super::image_paint::AnimationSlotSink<'_>,
    window: &mut Window,
    cx: &mut App,
) {
    for cmd in commands {
        let position = origin + point(px(cmd.x), px(cmd.y));
        let live_overlay = cmd.live_seventv_paint.as_deref().and_then(|spec| {
            seventv_paint::overlay_for_line(
                spec,
                &cmd.shaped_line,
                f32::from(cmd.shaped_line.font_size),
                line_height,
                window,
            )
        });
        let overlay = cmd.seventv_overlay.as_ref().or(live_overlay.as_ref());

        if overlay.is_none()
            && cmd.has_seventv_paint
            && let Some(data) = layout.shaped_lines.get(cmd.line_idx)
        {
            if cmd
                .live_seventv_paint
                .as_deref()
                .is_some_and(|paint| paint.is_animated())
            {
                animation_slots.slots.unslotted |= UNSLOTTED_NAME_PAINT;
            }
            let font_size = f32::from(cmd.shaped_line.font_size);
            paint_line_with_unpainted_name(data, position, font_size, line_height, window, cx);
        } else {
            let _ =
                cmd.shaped_line
                    .paint(position, px(line_height), TextAlign::Left, None, window, cx);
            match (live_overlay.as_ref(), cmd.live_seventv_paint.as_ref()) {
                (Some(overlay), Some(spec)) if cmd.seventv_overlay.is_none() => {
                    animation_slots.paint_name_overlay(
                        overlay,
                        spec,
                        &cmd.shaped_line,
                        line_height,
                        layout,
                        cmd.line_idx,
                        position,
                        window,
                    );
                }
                _ => {
                    if let Some(overlay) = overlay {
                        overlay.paint(position, window);
                    }
                }
            }
        }
        crate::startup_metrics::record("first_text_paint");
    }
}
