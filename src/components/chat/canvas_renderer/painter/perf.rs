//! Per-frame and aggregate render/layout counters.

pub(super) const REBUILD_REASON_ORDER_MISMATCH: u8 = 1 << 0;
pub(super) const REBUILD_REASON_HEIGHT_MISMATCH: u8 = 1 << 1;
pub(super) const REBUILD_REASON_STALE_LAYOUTS: u8 = 1 << 2;
pub(super) const REBUILD_REASON_FORCE_REFRESH: u8 = 1 << 3;

#[derive(Clone, Copy, Debug, Default)]
pub(super) struct RenderPerfFrameStats {
    pub(super) layout_cache_hits: u32,
    pub(super) layout_cache_misses: u32,
    /// Layout satisfied from the per-split local LRU cache.
    pub(super) layout_hit_split_cache: u32,
    /// Layout satisfied from the cross-split GlobalLayoutCache.
    pub(super) layout_hit_global_cache: u32,
    /// Layout satisfied by promoting a freshly-completed async result.
    pub(super) layout_hit_async_ready: u32,
    /// Layout built synchronously on the main thread (worst-case miss).
    pub(super) layout_miss_sync_shape: u32,
    pub(super) async_layout_requests: u32,
    pub(super) async_ready_valid_width: u32,
    pub(super) async_ready_stale_width: u32,
    pub(super) animation_visible_row_reuse: u32,
    pub(super) stable_visible_row_reuse: u32,
    pub(super) viewport_canvas_fast_path: u32,
    pub(super) viewport_canvas_prepared_snapshot_reuse: u32,
    pub(super) viewport_canvas_prepared_row_reuse: u32,
    pub(super) viewport_canvas_scroll_snapshot_reuse: u32,
    pub(super) viewport_canvas_prepared_text_lines: u32,
    pub(super) viewport_canvas_prepared_static_images: u32,
    pub(super) height_anchor_adjustments: u32,
    pub(super) full_layout_rebuilds: u32,
    pub(super) rebuild_reason_flags: u8,
    pub(super) message_order_resets: u32,
    pub(super) last_order_reset_reason: Option<&'static str>,
    /// Failed stable-reuse gate terms this frame (diagnostics only).
    pub(super) stable_gate_failures: u32,
    pub(super) row_paint_paths: RowPaintPathTally,
}

/// Element-fallback reasons, in `element_render::FALLBACK_*` bit order.
const ELEMENT_FALLBACK_REASON_NAMES: [&str; 8] = [
    "deleted",
    "svg",
    "animated_not_loaded",
    "animated_loading",
    "inline_box",
    "reply_indicator",
    "embed_preview",
    "upload_preview",
];

/// Visible rows painted by the direct canvas versus GPUI element fallback.
#[derive(Clone, Copy, Debug, Default)]
pub(super) struct RowPaintPathTally {
    pub(super) direct_rows: u32,
    pub(super) fallback_rows: u32,
    /// Fallback rows per reason; a row with several reasons counts in each.
    pub(super) fallback_reasons: [u32; 8],
}

impl RowPaintPathTally {
    #[inline]
    pub(super) fn record(&mut self, fallback_reasons: u8) {
        if fallback_reasons == 0 {
            self.direct_rows += 1;
            return;
        }
        self.fallback_rows += 1;
        for (bit, count) in self.fallback_reasons.iter_mut().enumerate() {
            if fallback_reasons & (1 << bit) != 0 {
                *count += 1;
            }
        }
    }
}

pub(super) fn element_fallback_reasons_label(counts: &[u64; 8]) -> String {
    counts_label(&ELEMENT_FALLBACK_REASON_NAMES, counts)
}

/// Stable-reuse gate terms, in `stable_frame_gate_failures` bit order.
const STABLE_GATE_TERM_NAMES: [&str; 17] = [
    "async_in_flight",
    "pending_width",
    "needs_rebuild",
    "pending_new",
    "stale_layouts",
    "fallback_metrics",
    "order_len",
    "height_len",
    "range",
    "width",
    "message_count",
    "scroll",
    "layout_settings",
    "invalidation",
    "eviction_seq",
    "channel_emotes",
    "scale_or_mod_mode",
];

pub(super) fn stable_gate_failures_label(counts: &[u64; 17]) -> String {
    counts_label(&STABLE_GATE_TERM_NAMES, counts)
}

fn counts_label(names: &[&str], counts: &[u64]) -> String {
    let mut label = String::new();
    for (name, count) in names.iter().zip(counts) {
        if *count == 0 {
            continue;
        }
        if !label.is_empty() {
            label.push('|');
        }
        label.push_str(name);
        label.push(':');
        label.push_str(&count.to_string());
    }
    if label.is_empty() {
        label.push_str("none");
    }
    label
}

#[derive(Debug, Default)]
pub(super) struct RenderPerfAggregateStats {
    pub(super) frames: u64,
    pub(super) layout_total_us: u128,
    pub(super) async_poll_total_us: u128,
    pub(super) elem_build_total_us: u128,
    pub(super) visible_rows_total: u64,
    pub(super) visible_span_total: u64,
    pub(super) completed_async_layouts_total: u64,
    pub(super) layout_cache_hits_total: u64,
    pub(super) layout_cache_misses_total: u64,
    pub(super) layout_hit_split_cache_total: u64,
    pub(super) layout_hit_global_cache_total: u64,
    pub(super) layout_hit_async_ready_total: u64,
    pub(super) layout_miss_sync_shape_total: u64,
    pub(super) async_layout_requests_total: u64,
    pub(super) async_ready_valid_width_total: u64,
    pub(super) async_ready_stale_width_total: u64,
    pub(super) animation_visible_row_reuse_total: u64,
    pub(super) animation_frame_unchanged_skips_total: u64,
    /// Animation-waker frame checks; with the unchanged skips, separates timer
    /// checks from the notifies that actually repaint.
    pub(super) animation_waker_checks_total: u64,
    /// Animation-waker frame changes by how they reached the screen, indexed by
    /// [`AnimationPatchOutcome`].
    pub(super) animation_patch_outcomes: [u64; 6],
    /// Unslotted outcomes by `UNSLOTTED_*` bit: animated image not loaded,
    /// loading, ready without a frame, failed; animated name paint.
    pub(super) animation_unslotted_reasons: [u64; 5],
    /// Failed patches by reason: no frame, no composite, geometry, rejected.
    pub(super) animation_patch_failures: [u64; 4],
    pub(super) direct_rows_total: u64,
    pub(super) element_fallback_rows_total: u64,
    pub(super) element_fallback_reason_counts: [u64; 8],
    pub(super) stable_visible_row_reuse_total: u64,
    pub(super) viewport_canvas_fast_path_total: u64,
    pub(super) viewport_canvas_prepared_snapshot_reuse_total: u64,
    pub(super) viewport_canvas_prepared_row_reuse_total: u64,
    pub(super) viewport_canvas_scroll_snapshot_reuse_total: u64,
    pub(super) viewport_canvas_prepared_text_lines_total: u64,
    pub(super) viewport_canvas_prepared_static_images_total: u64,
    pub(super) height_anchor_adjustments_total: u64,
    pub(super) full_layout_rebuilds_total: u64,
    pub(super) message_order_resets_total: u64,
    pub(super) rebuild_reason_order_total: u64,
    pub(super) rebuild_reason_height_total: u64,
    pub(super) rebuild_reason_stale_total: u64,
    pub(super) rebuild_reason_force_refresh_total: u64,
    pub(super) last_rebuild_reason_flags: u8,
    pub(super) last_order_reset_reason: Option<&'static str>,
    pub(super) stable_gate_fail_mask: u32,
    pub(super) stable_gate_fail_counts: [u64; 17],
}

pub(super) fn rebuild_reason_flags_label(flags: u8) -> String {
    if flags == 0 {
        return "none".to_string();
    }

    let mut reasons = Vec::with_capacity(4);
    if flags & REBUILD_REASON_ORDER_MISMATCH != 0 {
        reasons.push("order");
    }
    if flags & REBUILD_REASON_HEIGHT_MISMATCH != 0 {
        reasons.push("height");
    }
    if flags & REBUILD_REASON_STALE_LAYOUTS != 0 {
        reasons.push("stale");
    }
    if flags & REBUILD_REASON_FORCE_REFRESH != 0 {
        reasons.push("force");
    }
    reasons.join("|")
}

impl RenderPerfAggregateStats {
    pub(super) fn record_animation_patch_outcome(&mut self, outcome: super::AnimationPatchOutcome) {
        self.animation_patch_outcomes[outcome.index()] += 1;
        if let super::AnimationPatchOutcome::PatchFailed(failure) = outcome {
            self.animation_patch_failures[failure as usize] += 1;
        }
        if let super::AnimationPatchOutcome::Unslotted(bits) = outcome {
            for (bit, count) in self.animation_unslotted_reasons.iter_mut().enumerate() {
                if bits & (1 << bit) != 0 {
                    *count += 1;
                }
            }
        }
    }
}
