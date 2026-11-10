//! Render/layout performance accounting helpers.

use super::*;

impl CanvasMessageList {
    pub(super) fn reset_render_perf_frame_stats(&mut self) {
        self.render_perf_frame_stats = RenderPerfFrameStats::default();
    }

    #[track_caller]
    #[inline]
    pub(super) fn note_message_order_reset(&mut self, reason: &'static str) {
        let prev_order_len = self.message_model.message_order.len();
        let prev_height_len = self.message_model.height_tree.len();
        let msg_count = self.message_model.messages.len();
        if reason == "update_layouts.empty_messages" && prev_order_len == 0 && prev_height_len == 0
        {
            return;
        }

        self.note_relayout_cause(reason);
        self.render_perf_frame_stats.message_order_resets += 1;
        self.render_perf_frame_stats.last_order_reset_reason = Some(reason);
        crate::log_perf_render!(
            debug,
            channel_id = %self.channel_id,
            reason,
            prev_order_len,
            prev_height_len,
            msg_count,
            pending_new_message_count = self.message_state.pending_new_message_count,
            "Message order invalidated/reset"
        );
    }

    #[inline]
    pub(super) fn note_full_layout_rebuild(&mut self, reason_flags: u8) {
        self.render_perf_frame_stats.full_layout_rebuilds += 1;
        self.render_perf_frame_stats.rebuild_reason_flags |= reason_flags;
        crate::backend::metrics::record_emote_event("layout.full_rebuild");
        if reason_flags & REBUILD_REASON_STALE_LAYOUTS != 0 {
            crate::backend::metrics::record_emote_event("layout.full_rebuild.stale_emotes");
        }
        if reason_flags & REBUILD_REASON_FORCE_REFRESH != 0 {
            crate::backend::metrics::record_emote_event("layout.full_rebuild.force_refresh");
        }
    }

    #[inline]
    pub(super) fn flush_render_perf_aggregate_if_due(
        &mut self,
        layout_time: std::time::Duration,
        async_poll_time: std::time::Duration,
        elem_build_time: std::time::Duration,
        visible_rows: usize,
        visible_span: usize,
        completed_async_layouts: usize,
    ) {
        if !crate::backend::metrics::is_enabled() && !crate::log::enabled("perf_render") {
            return;
        }

        crate::backend::metrics::record_layout_frame(crate::backend::metrics::LayoutFrameMetrics {
            cache_hits: self.render_perf_frame_stats.layout_cache_hits as u64,
            cache_misses: self.render_perf_frame_stats.layout_cache_misses as u64,
            hit_split_cache: self.render_perf_frame_stats.layout_hit_split_cache as u64,
            hit_global_cache: self.render_perf_frame_stats.layout_hit_global_cache as u64,
            hit_async_ready: self.render_perf_frame_stats.layout_hit_async_ready as u64,
            miss_sync_shape: self.render_perf_frame_stats.layout_miss_sync_shape as u64,
            async_requests: self.render_perf_frame_stats.async_layout_requests as u64,
            async_ready_valid_width: self.render_perf_frame_stats.async_ready_valid_width as u64,
            async_ready_stale_width: self.render_perf_frame_stats.async_ready_stale_width as u64,
            full_rebuilds: self.render_perf_frame_stats.full_layout_rebuilds as u64,
            message_order_resets: self.render_perf_frame_stats.message_order_resets as u64,
            rebuild_reason_order: self.render_perf_frame_stats.rebuild_reason_flags
                & REBUILD_REASON_ORDER_MISMATCH
                != 0,
            rebuild_reason_height: self.render_perf_frame_stats.rebuild_reason_flags
                & REBUILD_REASON_HEIGHT_MISMATCH
                != 0,
            rebuild_reason_stale: self.render_perf_frame_stats.rebuild_reason_flags
                & REBUILD_REASON_STALE_LAYOUTS
                != 0,
            rebuild_reason_force: self.render_perf_frame_stats.rebuild_reason_flags
                & REBUILD_REASON_FORCE_REFRESH
                != 0,
        });

        {
            let agg = &mut self.render_perf_agg_stats;
            agg.frames += 1;
            agg.layout_total_us += layout_time.as_micros();
            agg.async_poll_total_us += async_poll_time.as_micros();
            agg.elem_build_total_us += elem_build_time.as_micros();
            agg.visible_rows_total += visible_rows as u64;
            agg.visible_span_total += visible_span as u64;
            agg.completed_async_layouts_total += completed_async_layouts as u64;
            agg.layout_cache_hits_total += self.render_perf_frame_stats.layout_cache_hits as u64;
            agg.layout_cache_misses_total +=
                self.render_perf_frame_stats.layout_cache_misses as u64;
            agg.layout_hit_split_cache_total +=
                self.render_perf_frame_stats.layout_hit_split_cache as u64;
            agg.layout_hit_global_cache_total +=
                self.render_perf_frame_stats.layout_hit_global_cache as u64;
            agg.layout_hit_async_ready_total +=
                self.render_perf_frame_stats.layout_hit_async_ready as u64;
            agg.layout_miss_sync_shape_total +=
                self.render_perf_frame_stats.layout_miss_sync_shape as u64;
            agg.async_layout_requests_total +=
                self.render_perf_frame_stats.async_layout_requests as u64;
            agg.async_ready_valid_width_total +=
                self.render_perf_frame_stats.async_ready_valid_width as u64;
            agg.async_ready_stale_width_total +=
                self.render_perf_frame_stats.async_ready_stale_width as u64;
            agg.animation_visible_row_reuse_total +=
                self.render_perf_frame_stats.animation_visible_row_reuse as u64;
            agg.stable_visible_row_reuse_total +=
                self.render_perf_frame_stats.stable_visible_row_reuse as u64;
            agg.viewport_canvas_fast_path_total +=
                self.render_perf_frame_stats.viewport_canvas_fast_path as u64;
            agg.viewport_canvas_prepared_snapshot_reuse_total +=
                self.render_perf_frame_stats
                    .viewport_canvas_prepared_snapshot_reuse as u64;
            agg.viewport_canvas_prepared_row_reuse_total +=
                self.render_perf_frame_stats
                    .viewport_canvas_prepared_row_reuse as u64;
            agg.viewport_canvas_scroll_snapshot_reuse_total +=
                self.render_perf_frame_stats
                    .viewport_canvas_scroll_snapshot_reuse as u64;
            agg.viewport_canvas_prepared_text_lines_total +=
                self.render_perf_frame_stats
                    .viewport_canvas_prepared_text_lines as u64;
            agg.viewport_canvas_prepared_static_images_total +=
                self.render_perf_frame_stats
                    .viewport_canvas_prepared_static_images as u64;
            agg.height_anchor_adjustments_total +=
                self.render_perf_frame_stats.height_anchor_adjustments as u64;
            agg.full_layout_rebuilds_total +=
                self.render_perf_frame_stats.full_layout_rebuilds as u64;
            agg.message_order_resets_total +=
                self.render_perf_frame_stats.message_order_resets as u64;
            if self.render_perf_frame_stats.rebuild_reason_flags & REBUILD_REASON_ORDER_MISMATCH
                != 0
            {
                agg.rebuild_reason_order_total += 1;
            }
            if self.render_perf_frame_stats.rebuild_reason_flags & REBUILD_REASON_HEIGHT_MISMATCH
                != 0
            {
                agg.rebuild_reason_height_total += 1;
            }
            if self.render_perf_frame_stats.rebuild_reason_flags & REBUILD_REASON_STALE_LAYOUTS != 0
            {
                agg.rebuild_reason_stale_total += 1;
            }
            if self.render_perf_frame_stats.rebuild_reason_flags & REBUILD_REASON_FORCE_REFRESH != 0
            {
                agg.rebuild_reason_force_refresh_total += 1;
            }
            if self.render_perf_frame_stats.rebuild_reason_flags != 0 {
                agg.last_rebuild_reason_flags = self.render_perf_frame_stats.rebuild_reason_flags;
            }
            if self
                .render_perf_frame_stats
                .last_order_reset_reason
                .is_some()
            {
                agg.last_order_reset_reason = self.render_perf_frame_stats.last_order_reset_reason;
            }
            let row_paint_paths = self.render_perf_frame_stats.row_paint_paths;
            agg.direct_rows_total += u64::from(row_paint_paths.direct_rows);
            agg.element_fallback_rows_total += u64::from(row_paint_paths.fallback_rows);
            for (total, count) in agg
                .element_fallback_reason_counts
                .iter_mut()
                .zip(row_paint_paths.fallback_reasons)
            {
                *total += u64::from(count);
            }
            let gate_failures = self.render_perf_frame_stats.stable_gate_failures;
            agg.stable_gate_fail_mask |= gate_failures;
            for (bit, count) in agg.stable_gate_fail_counts.iter_mut().enumerate() {
                if gate_failures & (1 << bit) != 0 {
                    *count += 1;
                }
            }
        }

        if self.last_render_perf_log.elapsed().as_millis() < RENDER_PERF_LOG_INTERVAL_MS {
            return;
        }

        {
            const PINNED_PROBE_SAMPLE: usize = 16;
            let mut sampled = 0u32;
            let mut local_only = 0u32;
            for key in self.layout_store.layouts.keys().take(PINNED_PROBE_SAMPLE) {
                sampled += 1;
                if !global_layout_cache::contains_layout(key) {
                    local_only += 1;
                }
            }
            if sampled > 0 {
                global_layout_cache::record_pinned_layout_probe(local_only, sampled);
            }
        }

        let (
            frames,
            avg_layout_ms,
            avg_async_poll_ms,
            avg_elem_ms,
            avg_visible_rows,
            avg_visible_span,
            avg_cache_hits,
            avg_cache_misses,
            hit_split_cache,
            hit_global_cache,
            hit_async_ready,
            miss_sync_shape,
            avg_async_requests,
            avg_async_completions,
            animation_visible_row_reuse,
            animation_frame_unchanged_skips,
            animation_waker_checks,
            stable_visible_row_reuse,
            viewport_canvas_fast_path,
            viewport_canvas_prepared_snapshot_reuse,
            viewport_canvas_prepared_row_reuse,
            viewport_canvas_scroll_snapshot_reuse,
            viewport_canvas_prepared_text_lines,
            viewport_canvas_prepared_static_images,
            height_anchor_adjustments,
            full_rebuilds,
            order_resets,
            rebuild_order,
            rebuild_height,
            rebuild_stale,
            rebuild_force,
            last_rebuild_reason,
            last_order_reset_reason,
            stable_gate_fail_mask,
            stable_gate_fail_terms,
            direct_rows,
            element_fallback_rows,
            element_fallback_terms,
            animation_patch_terms,
        ) = {
            let agg = &self.render_perf_agg_stats;
            let frames = agg.frames.max(1);
            (
                agg.frames,
                agg.layout_total_us as f64 / frames as f64 / 1000.0,
                agg.async_poll_total_us as f64 / frames as f64 / 1000.0,
                agg.elem_build_total_us as f64 / frames as f64 / 1000.0,
                agg.visible_rows_total as f64 / frames as f64,
                agg.visible_span_total as f64 / frames as f64,
                agg.layout_cache_hits_total as f64 / frames as f64,
                agg.layout_cache_misses_total as f64 / frames as f64,
                agg.layout_hit_split_cache_total,
                agg.layout_hit_global_cache_total,
                agg.layout_hit_async_ready_total,
                agg.layout_miss_sync_shape_total,
                agg.async_layout_requests_total as f64 / frames as f64,
                agg.completed_async_layouts_total as f64 / frames as f64,
                agg.animation_visible_row_reuse_total,
                agg.animation_frame_unchanged_skips_total,
                agg.animation_waker_checks_total,
                agg.stable_visible_row_reuse_total,
                agg.viewport_canvas_fast_path_total,
                agg.viewport_canvas_prepared_snapshot_reuse_total,
                agg.viewport_canvas_prepared_row_reuse_total,
                agg.viewport_canvas_scroll_snapshot_reuse_total,
                agg.viewport_canvas_prepared_text_lines_total,
                agg.viewport_canvas_prepared_static_images_total,
                agg.height_anchor_adjustments_total,
                agg.full_layout_rebuilds_total,
                agg.message_order_resets_total,
                agg.rebuild_reason_order_total,
                agg.rebuild_reason_height_total,
                agg.rebuild_reason_stale_total,
                agg.rebuild_reason_force_refresh_total,
                rebuild_reason_flags_label(agg.last_rebuild_reason_flags),
                agg.last_order_reset_reason.unwrap_or("none"),
                agg.stable_gate_fail_mask,
                stable_gate_failures_label(&agg.stable_gate_fail_counts),
                agg.direct_rows_total,
                agg.element_fallback_rows_total,
                element_fallback_reasons_label(&agg.element_fallback_reason_counts),
                animation_patch_outcomes_label(
                    &agg.animation_patch_outcomes,
                    &agg.animation_unslotted_reasons,
                    &agg.animation_patch_failures,
                ),
            )
        };
        let layout_worker = crate::components::chat::canvas_renderer::layout_worker_diagnostics();
        let split_queue_wait = self.async_layout_manager.take_queue_wait();
        let split_queue_wait_avg_us = split_queue_wait
            .sum_us
            .checked_div(split_queue_wait.count)
            .unwrap_or(0);

        crate::log_perf_render!(
            debug,
            channel_id = %self.channel_id,
            frames,
            avg_layout_ms,
            avg_async_poll_ms,
            avg_elem_ms,
            avg_visible_rows,
            avg_visible_span,
            avg_cache_hits,
            avg_cache_misses,
            hit_split_cache,
            hit_global_cache,
            hit_async_ready,
            miss_sync_shape,
            avg_async_requests,
            avg_async_completions,
            animation_visible_row_reuse,
            animation_frame_unchanged_skips,
            animation_waker_checks,
            stable_visible_row_reuse,
            viewport_canvas_fast_path,
            viewport_canvas_prepared_snapshot_reuse,
            viewport_canvas_prepared_row_reuse,
            viewport_canvas_scroll_snapshot_reuse,
            viewport_canvas_prepared_text_lines,
            viewport_canvas_prepared_static_images,
            height_anchor_adjustments,
            full_rebuilds,
            order_resets,
            rebuild_order,
            rebuild_height,
            rebuild_stale,
            rebuild_force,
            last_rebuild_reason = %last_rebuild_reason,
            last_order_reset_reason,
            stable_gate_fail_mask = format_args!("{stable_gate_fail_mask:#06x}"),
            stable_gate_fail_terms = %stable_gate_fail_terms,
            direct_rows,
            element_fallback_rows,
            element_fallback_terms = %element_fallback_terms,
            animation_patch_terms = %animation_patch_terms,
            queue_depth = layout_worker.queue_depth,
            queue_capacity = layout_worker.queue_capacity,
            queue_peak_depth = layout_worker.queue_peak_depth,
            queue_wait_max_us = layout_worker.queue_wait_max_us,
            enqueued_total = layout_worker.enqueued_total,
            queue_full_total = layout_worker.queue_full_total,
            queue_disconnected_total = layout_worker.queue_disconnected_total,
            worker_panic_total = layout_worker.worker_panic_total,
            shaped_total = layout_worker.shaped_total,
            published_total = layout_worker.published_total,
            obsolete_before_shape_total = layout_worker.obsolete_before_shape_total,
            obsolete_after_shape_total = layout_worker.obsolete_after_shape_total,
            layout_worker_scope = "process",
            split_queue_wait_jobs = split_queue_wait.count,
            split_queue_wait_avg_us,
            split_queue_wait_max_us = split_queue_wait.max_us,
            "Canvas render/layout aggregate"
        );

        self.last_render_perf_log = std::time::Instant::now();
        self.render_perf_agg_stats = RenderPerfAggregateStats::default();
    }
}

/// `patched:N fallback_rows:N unslotted:N no_slots:N patch_failed:N`, then the
/// unslotted reasons.
fn animation_patch_outcomes_label(
    counts: &[u64; 6],
    unslotted: &[u64; 5],
    failures: &[u64; 4],
) -> String {
    let outcomes = AnimationPatchOutcome::ALL
        .iter()
        .map(|outcome| format!("{}:{}", outcome.label(), counts[outcome.index()]));
    let reasons = [
        "img_not_loaded",
        "img_loading",
        "img_ready_no_frame",
        "img_failed",
        "name_paint",
    ]
    .iter()
    .zip(unslotted)
    .map(|(reason, count)| format!("{reason}:{count}"));
    let failures = [
        "fail_no_frame",
        "fail_no_composite",
        "fail_geometry",
        "fail_rejected",
    ]
    .iter()
    .zip(failures)
    .map(|(reason, count)| format!("{reason}:{count}"));
    outcomes
        .chain(reasons)
        .chain(failures)
        .collect::<Vec<_>>()
        .join(" ")
}
