//! Env-gated attribution of visible row re-spacing (`ECHO_RELAYOUT_TRACE=1`).
//!
//! Invalidation sites record their `#[track_caller]` provenance. Frames that
//! replace a visible row's layout compare its geometry with the previous frame
//! and log `relayout.visible` with the deltas and the latest causes. Every entry
//! point returns after one flag check when the variable is unset.

use super::layout::{CanvasMessageLayout, ImageType, LayoutSegment};
use gpui::SharedString;
use parking_lot::Mutex;
use rustc_hash::FxHashMap;
use std::hash::{Hash, Hasher};
use std::panic::Location;
use std::sync::{Arc, LazyLock};
use std::time::Instant;

static RELAYOUT_TRACE_ENABLED: LazyLock<bool> =
    LazyLock::new(|| echo_core::env::flag("ECHO_RELAYOUT_TRACE"));

static LAST_GLOBAL_CAUSE: Mutex<Option<RelayoutCause>> = Mutex::new(None);

#[inline]
pub fn enabled() -> bool {
    *RELAYOUT_TRACE_ENABLED
}

#[derive(Clone, Copy, Debug)]
pub struct RelayoutCause {
    kind: &'static str,
    location: &'static Location<'static>,
    at: Instant,
}

impl RelayoutCause {
    #[track_caller]
    pub fn here(kind: &'static str) -> Self {
        Self {
            kind,
            location: Location::caller(),
            at: Instant::now(),
        }
    }
}

impl std::fmt::Display for RelayoutCause {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}@{}:{}(age_ms={})",
            self.kind,
            self.location.file(),
            self.location.line(),
            self.at.elapsed().as_millis()
        )
    }
}

/// Record a process-wide layout invalidation at the caller's location.
#[track_caller]
#[inline]
pub fn note_layout_invalidation(kind: &'static str, ids: usize) {
    if !enabled() {
        return;
    }
    let cause = RelayoutCause::here(kind);
    *LAST_GLOBAL_CAUSE.lock() = Some(cause);
    crate::log_perf_render!(debug, ids, "relayout.invalidate cause={}", cause);
}

fn last_global_cause() -> Option<RelayoutCause> {
    *LAST_GLOBAL_CAUSE.lock()
}

#[derive(Clone, Copy, PartialEq)]
struct RowGeometry {
    layout_ptr: usize,
    height: f32,
    fingerprint: u64,
    window_metrics: bool,
    pending_dims: usize,
    badges: usize,
    inline_boxes: usize,
}

impl RowGeometry {
    fn of(layout: &Arc<CanvasMessageLayout>) -> Self {
        let half_px = |value: f32| (value * 2.0).round() as i32;
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        layout.line_count.hash(&mut hasher);
        let mut badges = 0;
        let mut inline_boxes = 0;
        for segment in &layout.segments {
            let (tag, x, y, width) = match segment {
                LayoutSegment::Text { x, y, width, .. } => (0u8, *x, *y, *width),
                LayoutSegment::Image {
                    x,
                    y,
                    width,
                    image_type,
                    ..
                } => {
                    if matches!(image_type, ImageType::Badge { .. }) {
                        badges += 1;
                    }
                    (1, *x, *y, *width)
                }
                LayoutSegment::LayeredImage { x, y, width, .. } => (2, *x, *y, *width),
                LayoutSegment::InlineBox { x, y, width, .. } => {
                    inline_boxes += 1;
                    (3, *x, *y, *width)
                }
                _ => continue,
            };
            (tag, half_px(x), half_px(y), half_px(width)).hash(&mut hasher);
        }
        Self {
            layout_ptr: Arc::as_ptr(layout) as usize,
            height: layout.height,
            fingerprint: hasher.finish(),
            window_metrics: layout.uses_window_text_metrics,
            pending_dims: layout.pending_native_dimension_urls.len(),
            badges,
            inline_boxes,
        }
    }
}

/// Visible-row geometry from the last frame that rebuilt or translated rows.
#[derive(Default)]
pub struct VisibleGeometryTrace {
    quantized_width: u32,
    show_timestamp: bool,
    rows: FxHashMap<SharedString, RowGeometry>,
}

impl VisibleGeometryTrace {
    /// Diff this frame's visible layouts against the previous traced frame and
    /// log each row whose geometry changed without a width or timestamp change.
    pub fn observe<'a>(
        &mut self,
        channel_id: &str,
        quantized_width: u32,
        show_timestamp: bool,
        local_cause: Option<RelayoutCause>,
        layouts: impl Iterator<Item = &'a Arc<CanvasMessageLayout>>,
    ) {
        let comparable =
            self.quantized_width == quantized_width && self.show_timestamp == show_timestamp;
        let mut previous = std::mem::take(&mut self.rows);
        for layout in layouts {
            let id = layout.message_id_shared.clone();
            let before = previous.remove(&id);
            let current = match before {
                Some(before) if before.layout_ptr == Arc::as_ptr(layout) as usize => before,
                _ => RowGeometry::of(layout),
            };
            if comparable
                && let Some(before) = before
                && before.layout_ptr != current.layout_ptr
                && (before.fingerprint != current.fingerprint
                    || (before.height - current.height).abs() > 0.25)
            {
                crate::log_perf_render!(
                    debug,
                    "relayout.visible channel={} msg={} h={:.1}->{:.1} fp={:x}->{:x} metrics={}->{} dims_pending={}->{} badges={}->{} inline={}->{} cause={} global={}",
                    channel_id,
                    id,
                    before.height,
                    current.height,
                    before.fingerprint,
                    current.fingerprint,
                    metrics_label(before.window_metrics),
                    metrics_label(current.window_metrics),
                    before.pending_dims,
                    current.pending_dims,
                    before.badges,
                    current.badges,
                    before.inline_boxes,
                    current.inline_boxes,
                    DisplayCause(local_cause),
                    DisplayCause(last_global_cause()),
                );
            }
            self.rows.insert(id, current);
        }
        self.quantized_width = quantized_width;
        self.show_timestamp = show_timestamp;
    }
}

fn metrics_label(window_metrics: bool) -> &'static str {
    if window_metrics { "window" } else { "fallback" }
}

struct DisplayCause(Option<RelayoutCause>);

impl std::fmt::Display for DisplayCause {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.0 {
            Some(cause) => cause.fmt(f),
            None => f.write_str("none"),
        }
    }
}
