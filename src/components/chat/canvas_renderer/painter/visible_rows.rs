//! Visible row metadata and per-frame visible-row build state.

use super::*;

/// Scroll direction for predictive emote preloading.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(super) enum ScrollDirection {
    /// User is scrolling down (content moving up, viewing newer messages).
    Down,
    /// User is scrolling up (content moving down, viewing older messages).
    Up,
    /// Viewport is static (no scrolling).
    Static,
}

/// Reusable per-frame metadata for visible messages with ready layouts.
pub(super) struct VisibleRenderRow {
    pub(super) layout: Arc<CanvasMessageLayout>,
    pub(super) message_index: usize,
    pub(super) viewport_y: f32,
    /// Zebra parity from the message's buffer-assigned flag, read fresh each
    /// row build so it never goes stale in cached layouts.
    pub(super) is_alternate_row: bool,
    pub(super) is_deleted: bool,
}

pub(super) struct VisibleRowBuildFrameState {
    pub(super) quantized_width: u32,
    pub(super) effective_layout_width: f32,
    pub(super) current_emote_version: u64,
    pub(super) viewport_anchor_start_idx: usize,
    pub(super) was_at_bottom_before_metric_refresh: bool,
    pub(super) fallback_metric_refreshes: usize,
    pub(super) visible_cache_miss_rebuilds: usize,
    pub(super) fallback_metric_heights_updated: bool,
    pub(super) visible_layouts_replaced: bool,
    pub(super) needs_notify: bool,
}
