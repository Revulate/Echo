use super::{
    EmotePickerAnimationMode, EmotePickerPopout, EmoteTab, LoadingState, VirtualRow, image_cache,
    picker_display_url, picker_native_animated_enabled, stability,
    visible_animated_emote_cell_count, visible_emote_cell_count,
};
use crate::components::overlays::emote_picker::grid;
use crate::components::overlays::emote_picker::signals::POPOUT_SIGNALS;
use gpui::{AppContext, Context, ScrollStrategy, Window};
use std::sync::Arc;
use std::time::{Duration, Instant};

const BENCH_STABILITY_MINIMUM_RANGE_OBSERVATIONS: u64 = 4;
const BENCH_ANIMATION_DIAGNOSTIC_ENTRY_LIMIT: usize = 32;
const BENCH_ANIMATION_CONTINUITY_MIN_MS: u128 = 15_000;
const BENCH_ANIMATION_CONTINUITY_MAX_MS: u128 = 30_000;
const BENCH_ANIMATION_CONTINUITY_MIN_TRANSITIONS: u64 = 2;
const BENCH_ANIMATION_CONTINUITY_MAX_STALL_MS: u128 = 10_000;

/// Benchmark-only, in-process traversal of a large picker catalog.
///
/// Keeping this driver inside the popout lets the live suite exercise the real
/// `UniformList` viewport/prefetch path without relying on foreground-window
/// focus or synthetic OS input.
pub(super) struct BenchPickerSweepState {
    channel: String,
    pub(super) animation_mode_override: Option<EmotePickerAnimationMode>,
    started_at: Instant,
    dwell: Duration,
    settle: Duration,
    stability_settle: Duration,
    stability_sample: Duration,
    stability_tick: Duration,
    overlap_rows: usize,
    last_emote_count: Option<usize>,
    last_render_version: Option<u32>,
    catalog_stable_since: Option<Instant>,
    start_logged: bool,
    maximize_requested: bool,
    maximized_logged: bool,
    task_started: bool,
    wake_scheduled: bool,
    retry_count: u8,
}

impl BenchPickerSweepState {
    pub(super) fn from_env() -> Option<Self> {
        let channel = echo_core::channel_identity::normalize_channel_key(&echo_core::env::value(
            "ECHO_BENCH_PICKER_SWEEP_CHANNEL",
        )?);
        if channel.is_empty() {
            return None;
        }
        let animation_mode_override = echo_core::env::value("ECHO_BENCH_PICKER_ANIMATION_MODE")
            .and_then(|value| match value.trim().to_ascii_lowercase().as_str() {
                "always" => Some(EmotePickerAnimationMode::Always),
                _ => None,
            });

        let dwell_ms = echo_core::env::parsed::<u64>("ECHO_BENCH_PICKER_SWEEP_DWELL_MS")
            .unwrap_or(180)
            .clamp(25, 5_000);
        let settle_ms = echo_core::env::parsed::<u64>("ECHO_BENCH_PICKER_SWEEP_SETTLE_MS")
            .unwrap_or(4_000)
            .clamp(500, 30_000);
        let overlap_rows = echo_core::env::parsed::<usize>("ECHO_BENCH_PICKER_SWEEP_OVERLAP_ROWS")
            .unwrap_or(3)
            .clamp(1, 32);
        let stability_settle_ms =
            echo_core::env::parsed::<u64>("ECHO_BENCH_PICKER_STABILITY_SETTLE_MS")
                .unwrap_or(5_000)
                .clamp(0, 30_000);
        let stability_sample_ms =
            echo_core::env::parsed::<u64>("ECHO_BENCH_PICKER_STABILITY_SAMPLE_MS")
                .unwrap_or(5_000)
                .clamp(1_000, 120_000);
        let stability_tick_ms =
            echo_core::env::parsed::<u64>("ECHO_BENCH_PICKER_STABILITY_TICK_MS")
                .unwrap_or(250)
                .clamp(25, stability_sample_ms);

        Some(Self {
            channel,
            animation_mode_override,
            started_at: Instant::now(),
            dwell: Duration::from_millis(dwell_ms),
            settle: Duration::from_millis(settle_ms),
            stability_settle: Duration::from_millis(stability_settle_ms),
            stability_sample: Duration::from_millis(stability_sample_ms),
            stability_tick: Duration::from_millis(stability_tick_ms),
            overlap_rows,
            last_emote_count: None,
            last_render_version: None,
            catalog_stable_since: None,
            start_logged: false,
            maximize_requested: false,
            maximized_logged: false,
            task_started: false,
            wake_scheduled: false,
            retry_count: 0,
        })
    }

    fn targets_channel(&self, channel: &str) -> bool {
        channel.eq_ignore_ascii_case(&self.channel)
    }
}

fn animation_mode_label(mode: EmotePickerAnimationMode) -> &'static str {
    match mode {
        EmotePickerAnimationMode::Always => "always",
        EmotePickerAnimationMode::Hover => "hover",
        EmotePickerAnimationMode::Never => "never",
    }
}

fn mark_sweep_visible_rows(visited: &mut [bool]) -> usize {
    let (start, end) = POPOUT_SIGNALS.visible_range();
    let start = start.min(visited.len());
    let end = end.min(visited.len());
    for was_visited in &mut visited[start..end] {
        *was_visited = true;
    }
    visited.iter().filter(|visited| **visited).count()
}

fn log_bench_animation_diagnostics(
    channel: &str,
    rows: &[VirtualRow],
    start: usize,
    end: usize,
    snapshot: &stability::PickerStabilitySnapshot,
) {
    let start = start.min(rows.len());
    let end = end.min(rows.len());
    let mut expected_advertised = 0usize;
    let mut ready = 0usize;
    let mut loading = 0usize;
    let mut failed = 0usize;
    let mut not_loaded = 0usize;
    let mut metadata_frames_zero = 0usize;
    let mut metadata_frames_one = 0usize;
    let mut metadata_frames_multi = 0usize;
    let mut advanced = 0usize;
    let mut unresolved = 0usize;
    let mut entries = Vec::with_capacity(BENCH_ANIMATION_DIAGNOSTIC_ENTRY_LIMIT);
    let compact_animations = super::super::prefetch::picker_uses_compact_animation_variant(
        visible_animated_emote_cell_count(rows, start, end),
        EmotePickerAnimationMode::Always,
    );

    for row in &rows[start..end] {
        let VirtualRow::EmoteRow { emotes, base_index } = row else {
            continue;
        };
        for (offset, (emote, _)) in emotes.iter().enumerate() {
            if !emote.is_animated {
                continue;
            }

            expected_advertised = expected_advertised.saturating_add(1);
            let cell_index = *base_index + offset;
            let primary_url = picker_display_url(emote);
            let url = super::super::prefetch::picker_active_animation_url(
                emote,
                primary_url,
                compact_animations,
            );
            let identity = crate::backend::types::media_source_id_for_url(emote, url);
            let state = image_cache::get_animated_cache_state_media(url, identity.as_ref());
            let active_loading_age_ms =
                image_cache::animated_decode_loading_age_ms_media(url, identity.as_ref())
                    .unwrap_or(0);
            let (state_label, state_ready) = match state {
                image_cache::AnimatedCacheState::Ready => {
                    ready = ready.saturating_add(1);
                    ("ready", true)
                }
                image_cache::AnimatedCacheState::Loading => {
                    loading = loading.saturating_add(1);
                    ("loading", false)
                }
                image_cache::AnimatedCacheState::Failed => {
                    failed = failed.saturating_add(1);
                    ("failed", false)
                }
                image_cache::AnimatedCacheState::NotLoaded => {
                    not_loaded = not_loaded.saturating_add(1);
                    ("not-loaded", false)
                }
            };
            let frame_count =
                image_cache::get_animation_metadata(url).map_or(0, |metadata| metadata.frame_count);
            match frame_count {
                0 => metadata_frames_zero = metadata_frames_zero.saturating_add(1),
                1 => metadata_frames_one = metadata_frames_one.saturating_add(1),
                _ => metadata_frames_multi = metadata_frames_multi.saturating_add(1),
            }
            let cell_advanced = snapshot.animated_cell_advanced(cell_index);
            if cell_advanced {
                advanced = advanced.saturating_add(1);
            }

            if !state_ready || frame_count < 2 || !cell_advanced {
                unresolved = unresolved.saturating_add(1);
                if entries.len() < BENCH_ANIMATION_DIAGNOSTIC_ENTRY_LIMIT {
                    entries.push((
                        cell_index,
                        emote.name.clone(),
                        state_label,
                        frame_count,
                        cell_advanced,
                        url.to_string(),
                        active_loading_age_ms,
                    ));
                }
            }
        }
    }

    crate::log_ui!(
        info,
        "bench.picker_stability diagnostics channel={} expected_advertised={} snapshot_expected={} ready={} loading={} failed={} not_loaded={} metadata_frames_zero={} metadata_frames_one={} metadata_frames_multi={} advanced={} frozen={} unresolved={} entries_logged={} entries_truncated={}",
        channel,
        expected_advertised,
        snapshot.expected_visible_animated_cells,
        ready,
        loading,
        failed,
        not_loaded,
        metadata_frames_zero,
        metadata_frames_one,
        metadata_frames_multi,
        advanced,
        expected_advertised.saturating_sub(advanced),
        unresolved,
        entries.len(),
        unresolved.saturating_sub(entries.len()),
    );
    for (cell_index, name, state, frame_count, cell_advanced, active_url, active_loading_age_ms) in
        entries
    {
        crate::log_ui!(
            warn,
            "bench.picker_stability diagnostic_entry channel={} index={} name={:?} state={} frame_count={} advanced={} active_loading_age_ms={} active_url={}",
            channel,
            cell_index,
            name,
            state,
            frame_count,
            cell_advanced,
            active_loading_age_ms,
            active_url,
        );
    }
}

fn bench_sweep_targets(row_count: usize, visible_span: usize, overlap_rows: usize) -> Vec<usize> {
    if row_count == 0 {
        return Vec::new();
    }

    let step = visible_span.saturating_sub(overlap_rows).max(1);
    let mut targets = Vec::with_capacity(row_count.div_ceil(step) + 1);
    let mut row = 0usize;
    while row < row_count {
        targets.push(row);
        row = row.saturating_add(step);
    }
    let last = row_count - 1;
    if targets.last().copied() != Some(last) {
        targets.push(last);
    }
    targets
}

fn bench_sweep_emote_coverage(rows: &[VirtualRow], visited: &[bool]) -> (usize, usize, usize) {
    let mut emote_rows_visited = 0usize;
    let mut emotes_visited = 0usize;
    let mut emotes_total = 0usize;
    for (index, row) in rows.iter().enumerate() {
        if let VirtualRow::EmoteRow { emotes, .. } = row {
            emotes_total += emotes.len();
            if visited.get(index).copied().unwrap_or(false) {
                emote_rows_visited += 1;
                emotes_visited += emotes.len();
            }
        }
    }
    (emote_rows_visited, emotes_visited, emotes_total)
}

impl EmotePickerPopout {
    fn schedule_bench_sweep_wake(&mut self, cx: &mut Context<Self>) {
        let Some(sweep) = self.bench_sweep.as_mut() else {
            return;
        };
        if sweep.wake_scheduled || sweep.task_started {
            return;
        }
        sweep.wake_scheduled = true;

        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(200))
                .await;
            this.update(cx, |picker, cx| {
                if let Some(sweep) = picker.bench_sweep.as_mut() {
                    sweep.wake_scheduled = false;
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    pub(super) fn drive_bench_picker_sweep(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(sweep) = self.bench_sweep.as_ref() else {
            return;
        };
        if !sweep.targets_channel(&self.grid.core.channel) || sweep.task_started {
            return;
        }

        let native_animated = picker_native_animated_enabled();
        if sweep.animation_mode_override != Some(EmotePickerAnimationMode::Always)
            || self.grid.animation_mode != EmotePickerAnimationMode::Always
            || native_animated
        {
            crate::log_ui!(
                error,
                "bench.picker_sweep failed channel={} reason=animation_policy animation_mode={} native_animated={}",
                self.grid.core.channel,
                animation_mode_label(self.grid.animation_mode),
                native_animated
            );
            if let Some(sweep) = self.bench_sweep.as_mut() {
                sweep.task_started = true;
            }
            window.remove_window();
            return;
        }

        if !sweep.start_logged {
            let channel = self.grid.core.channel.clone();
            crate::log_ui!(
                info,
                "bench.picker_sweep start channel={} requested_tab=Channel animation_mode={} native_animated={}",
                channel,
                animation_mode_label(self.grid.animation_mode),
                native_animated,
            );
            if let Some(sweep) = self.bench_sweep.as_mut() {
                sweep.start_logged = true;
            }
        }

        let maximize_requested = self
            .bench_sweep
            .as_ref()
            .is_some_and(|sweep| sweep.maximize_requested);
        if !maximize_requested {
            if let Some(sweep) = self.bench_sweep.as_mut() {
                sweep.maximize_requested = true;
            }
            if !window.is_maximized() {
                window.zoom_window();
            }
            self.schedule_bench_sweep_wake(cx);
            return;
        }

        if !window.is_maximized() {
            self.schedule_bench_sweep_wake(cx);
            return;
        }

        if self
            .bench_sweep
            .as_ref()
            .is_some_and(|sweep| !sweep.maximized_logged)
        {
            crate::log_ui!(
                info,
                "bench.picker_sweep maximized channel={} maximized=true",
                self.grid.core.channel
            );
            if let Some(sweep) = self.bench_sweep.as_mut() {
                sweep.maximized_logged = true;
            }
        }

        if self.grid.core.active_tab != EmoteTab::Channel {
            grid::select_tab(self, EmoteTab::Channel, cx);
            self.schedule_bench_sweep_wake(cx);
            return;
        }

        let timed_out = self
            .bench_sweep
            .as_ref()
            .is_some_and(|sweep| sweep.started_at.elapsed() >= Duration::from_secs(60));
        let loading = self.grid.core.get_loading_state();
        if timed_out
            && (loading != LoadingState::Loaded
                || self.grid.total_available_emotes == 0
                || self.grid.visible_emote_count != self.grid.total_available_emotes)
        {
            crate::log_ui!(
                error,
                "bench.picker_sweep failed channel={} reason=readiness_timeout loading={:?} shown={} total={} rows={}",
                self.grid.core.channel,
                loading,
                self.grid.visible_emote_count,
                self.grid.total_available_emotes,
                self.grid.virtual_rows.len()
            );
            if let Some(sweep) = self.bench_sweep.as_mut() {
                sweep.task_started = true;
            }
            window.remove_window();
            return;
        }

        if loading != LoadingState::Loaded
            || self.grid.total_available_emotes == 0
            || self.grid.virtual_rows.is_empty()
        {
            self.schedule_bench_sweep_wake(cx);
            return;
        }

        if self.grid.visible_emote_count != self.grid.total_available_emotes {
            crate::log_ui!(
                error,
                "bench.picker_sweep failed channel={} reason=catalog_paginated shown={} total={} limit={}",
                self.grid.core.channel,
                self.grid.visible_emote_count,
                self.grid.total_available_emotes,
                self.grid.visible_emote_limit
            );
            if let Some(sweep) = self.bench_sweep.as_mut() {
                sweep.task_started = true;
            }
            window.remove_window();
            return;
        }

        let now = Instant::now();
        let catalog_stable = {
            let sweep = self
                .bench_sweep
                .as_mut()
                .expect("bench sweep exists while being driven");
            if sweep.last_emote_count != Some(self.grid.total_available_emotes)
                || sweep.last_render_version != Some(self.grid.render_version)
            {
                sweep.last_emote_count = Some(self.grid.total_available_emotes);
                sweep.last_render_version = Some(self.grid.render_version);
                sweep.catalog_stable_since = Some(now);
                false
            } else {
                sweep
                    .catalog_stable_since
                    .is_some_and(|since| now.duration_since(since) >= sweep.settle)
            }
        };
        if !catalog_stable {
            self.schedule_bench_sweep_wake(cx);
            return;
        }

        let (visible_start, visible_end) = POPOUT_SIGNALS.visible_range();
        if visible_end <= visible_start {
            self.schedule_bench_sweep_wake(cx);
            return;
        }

        self.start_bench_picker_sweep(window, visible_end - visible_start, cx);
    }

    /// Return true when repeated live catalog churn exhausted the bounded
    /// retry budget and the benchmark should fail/close.
    fn restart_bench_sweep_after_catalog_change(&mut self, cx: &mut Context<Self>) -> bool {
        stability::cancel_stationary_window();
        let Some(sweep) = self.bench_sweep.as_mut() else {
            return true;
        };
        sweep.retry_count = sweep.retry_count.saturating_add(1);
        if sweep.retry_count > 3 {
            return true;
        }

        sweep.task_started = false;
        sweep.last_emote_count = None;
        sweep.last_render_version = None;
        sweep.catalog_stable_since = None;
        sweep.wake_scheduled = false;
        cx.notify();
        false
    }

    fn start_bench_picker_sweep(
        &mut self,
        window: &Window,
        visible_span: usize,
        cx: &mut Context<Self>,
    ) {
        let Some(sweep) = self.bench_sweep.as_mut() else {
            return;
        };
        if sweep.task_started {
            return;
        }
        sweep.task_started = true;

        let channel = self.grid.core.channel.clone();
        let dwell = sweep.dwell;
        let stability_settle = sweep.stability_settle;
        let stability_sample = sweep.stability_sample;
        let stability_tick = sweep.stability_tick;
        let overlap_rows = sweep.overlap_rows;
        let row_count = self.grid.virtual_rows.len();
        let rows = Arc::clone(&self.grid.virtual_rows);
        let render_version = self.grid.render_version;
        let animation_mode_name = animation_mode_label(self.grid.animation_mode);
        let native_animated = picker_native_animated_enabled();
        let window_handle = window.window_handle();
        let targets = bench_sweep_targets(row_count, visible_span, overlap_rows);
        let shown = self.grid.visible_emote_count;

        crate::log_ui!(
            info,
            "bench.picker_sweep ready channel={} tab=Channel maximized=true emotes={} rows={} visible_span={} overlap_rows={}",
            channel,
            shown,
            row_count,
            visible_span,
            overlap_rows
        );

        cx.spawn(async move |this, cx| {
            let mut forward_visited = vec![false; row_count];
            let mut forward_steps = 0usize;
            for target in targets.iter().copied() {
                let current = this
                    .update(cx, |picker, cx| {
                        if picker.grid.render_version != render_version {
                            return false;
                        }
                        picker
                            .grid.scroll_handle
                            .scroll_to_item(target, ScrollStrategy::Nearest);
                        cx.notify();
                        true
                    })
                    .unwrap_or(false);
                if !current {
                    let exhausted = this
                        .update(cx, |picker, cx| {
                            picker.restart_bench_sweep_after_catalog_change(cx)
                        })
                        .unwrap_or(true);
                    if exhausted {
                        crate::log_ui!(error, "bench.picker_sweep failed channel={} reason=catalog_churn retries_exhausted=true pass=forward", channel);
                        cx.update_window(window_handle, |_view, window, _cx| {
                            window.remove_window();
                        })
                        .ok();
                    } else {
                        crate::log_ui!(warn, "bench.picker_sweep restart channel={} reason=catalog_changed pass=forward", channel);
                    }
                    return;
                }
                cx.background_executor().timer(dwell).await;
                mark_sweep_visible_rows(&mut forward_visited);
                forward_steps += 1;
            }

            let forward_missing = rows
                .iter()
                .enumerate()
                .filter_map(|(index, row)| {
                    matches!(row, VirtualRow::EmoteRow { .. })
                        .then_some(index)
                        .filter(|index| !forward_visited[*index])
                })
                .collect::<Vec<_>>();
            for target in forward_missing {
                let current = this
                    .update(cx, |picker, cx| {
                        if picker.grid.render_version != render_version {
                            return false;
                        }
                        picker
                            .grid.scroll_handle
                            .scroll_to_item(target, ScrollStrategy::Nearest);
                        cx.notify();
                        true
                    })
                    .unwrap_or(false);
                if !current {
                    let exhausted = this
                        .update(cx, |picker, cx| {
                            picker.restart_bench_sweep_after_catalog_change(cx)
                        })
                        .unwrap_or(true);
                    if exhausted {
                        crate::log_ui!(error, "bench.picker_sweep failed channel={} reason=catalog_churn retries_exhausted=true pass=forward_repair", channel);
                        cx.update_window(window_handle, |_view, window, _cx| {
                            window.remove_window();
                        })
                        .ok();
                    } else {
                        crate::log_ui!(warn, "bench.picker_sweep restart channel={} reason=catalog_changed pass=forward_repair", channel);
                    }
                    return;
                }
                cx.background_executor().timer(dwell).await;
                mark_sweep_visible_rows(&mut forward_visited);
                forward_steps += 1;
            }

            let (forward_rows, forward_emotes, forward_total) =
                bench_sweep_emote_coverage(rows.as_slice(), &forward_visited);
            let forward_complete = forward_emotes == forward_total && forward_total == shown;
            crate::log_ui!(
                info,
                "bench.picker_sweep forward_complete channel={} coverage_complete={} emotes_visited={} emotes_total={} emote_rows_visited={} steps={}",
                channel,
                forward_complete,
                forward_emotes,
                forward_total,
                forward_rows,
                forward_steps
            );

            let mut reverse_visited = vec![false; row_count];
            let mut reverse_steps = 0usize;
            for target in targets.iter().rev().copied() {
                let current = this
                    .update(cx, |picker, cx| {
                        if picker.grid.render_version != render_version {
                            return false;
                        }
                        picker
                            .grid.scroll_handle
                            .scroll_to_item(target, ScrollStrategy::Nearest);
                        cx.notify();
                        true
                    })
                    .unwrap_or(false);
                if !current {
                    let exhausted = this
                        .update(cx, |picker, cx| {
                            picker.restart_bench_sweep_after_catalog_change(cx)
                        })
                        .unwrap_or(true);
                    if exhausted {
                        crate::log_ui!(error, "bench.picker_sweep failed channel={} reason=catalog_churn retries_exhausted=true pass=reverse", channel);
                        cx.update_window(window_handle, |_view, window, _cx| {
                            window.remove_window();
                        })
                        .ok();
                    } else {
                        crate::log_ui!(warn, "bench.picker_sweep restart channel={} reason=catalog_changed pass=reverse", channel);
                    }
                    return;
                }
                cx.background_executor().timer(dwell).await;
                mark_sweep_visible_rows(&mut reverse_visited);
                reverse_steps += 1;
            }

            let reverse_missing = rows
                .iter()
                .enumerate()
                .rev()
                .filter_map(|(index, row)| {
                    matches!(row, VirtualRow::EmoteRow { .. })
                        .then_some(index)
                        .filter(|index| !reverse_visited[*index])
                })
                .collect::<Vec<_>>();
            for target in reverse_missing {
                let current = this
                    .update(cx, |picker, cx| {
                        if picker.grid.render_version != render_version {
                            return false;
                        }
                        picker
                            .grid.scroll_handle
                            .scroll_to_item(target, ScrollStrategy::Nearest);
                        cx.notify();
                        true
                    })
                    .unwrap_or(false);
                if !current {
                    let exhausted = this
                        .update(cx, |picker, cx| {
                            picker.restart_bench_sweep_after_catalog_change(cx)
                        })
                        .unwrap_or(true);
                    if exhausted {
                        crate::log_ui!(error, "bench.picker_sweep failed channel={} reason=catalog_churn retries_exhausted=true pass=reverse_repair", channel);
                        cx.update_window(window_handle, |_view, window, _cx| {
                            window.remove_window();
                        })
                        .ok();
                    } else {
                        crate::log_ui!(warn, "bench.picker_sweep restart channel={} reason=catalog_changed pass=reverse_repair", channel);
                    }
                    return;
                }
                cx.background_executor().timer(dwell).await;
                mark_sweep_visible_rows(&mut reverse_visited);
                reverse_steps += 1;
            }

            let (reverse_rows, reverse_emotes, reverse_total) =
                bench_sweep_emote_coverage(rows.as_slice(), &reverse_visited);
            let reverse_complete = reverse_emotes == reverse_total && reverse_total == shown;
            crate::log_ui!(
                info,
                "bench.picker_sweep reverse_complete channel={} coverage_complete={} emotes_visited={} emotes_total={} emote_rows_visited={} steps={}",
                channel,
                reverse_complete,
                reverse_emotes,
                reverse_total,
                reverse_rows,
                reverse_steps
            );

            let coverage_complete = forward_complete && reverse_complete;
            if coverage_complete {
                crate::log_ui!(
                    info,
                    "bench.picker_stability settle channel={} stationary=true duration_ms={} tick_ms={}",
                    channel,
                    stability_settle.as_millis(),
                    stability_tick.as_millis()
                );
                let settle_tick_count = if stability_settle.is_zero() {
                    0
                } else {
                    stability_settle
                        .as_millis()
                        .div_ceil(stability_tick.as_millis()) as usize
                };
                for _ in 0..settle_tick_count {
                    let current = this
                        .update(cx, |picker, cx| {
                            if picker.grid.render_version != render_version {
                                return false;
                            }
                            cx.notify();
                            true
                        })
                        .unwrap_or(false);
                    if !current {
                        let exhausted = this
                            .update(cx, |picker, cx| {
                                picker.restart_bench_sweep_after_catalog_change(cx)
                            })
                            .unwrap_or(true);
                        if exhausted {
                            crate::log_ui!(error, "bench.picker_stability failed channel={} reason=catalog_churn retries_exhausted=true phase=settle", channel);
                            cx.update_window(window_handle, |_view, window, _cx| {
                                window.remove_window();
                            })
                            .ok();
                        } else {
                            crate::log_ui!(warn, "bench.picker_sweep restart channel={} reason=catalog_changed pass=stability_settle", channel);
                        }
                        return;
                    }
                    cx.background_executor().timer(stability_tick).await;
                }

                let stationary_viewport = this
                    .update(cx, |picker, cx| {
                        if picker.grid.render_version != render_version {
                            return None;
                        }
                        let (visible_start, visible_end) = POPOUT_SIGNALS.visible_range();
                        let expected_visible_cells = visible_emote_cell_count(
                            picker.grid.virtual_rows.as_slice(),
                            visible_start,
                            visible_end,
                        );
                        let expected_visible_animated_cells = visible_animated_emote_cell_count(
                            picker.grid.virtual_rows.as_slice(),
                            visible_start,
                            visible_end,
                        );
                        if expected_visible_cells == 0
                            || expected_visible_animated_cells == 0
                            || visible_end <= visible_start
                        {
                            return None;
                        }
                        stability::begin_stationary_window(
                            expected_visible_cells,
                            expected_visible_animated_cells,
                            visible_start,
                            visible_end,
                        );
                        cx.notify();
                        Some((
                            expected_visible_cells,
                            expected_visible_animated_cells,
                            visible_start,
                            visible_end,
                        ))
                    })
                    .ok()
                    .flatten();

                let Some((
                    expected_visible_cells,
                    expected_visible_animated_cells,
                    visible_start,
                    visible_end,
                )) = stationary_viewport
                else {
                    crate::log_ui!(error, "bench.picker_stability failed channel={} reason=no_stationary_viewport", channel);
                    cx.update_window(window_handle, |_view, window, _cx| {
                        window.remove_window();
                    })
                    .ok();
                    return;
                };

                crate::log_ui!(
                    info,
                    "bench.picker_stability start channel={} stationary=true animation_mode={} native_animated={} settle_ms={} timeout_ms={} tick_ms={} expected_visible_cells={} expected_visible_animated_cells={} visible_start={} visible_end={}",
                    channel,
                    animation_mode_name,
                    native_animated,
                    stability_settle.as_millis(),
                    stability_sample.as_millis(),
                    stability_tick.as_millis(),
                    expected_visible_cells,
                    expected_visible_animated_cells,
                    visible_start,
                    visible_end
                );

                let readiness_started_at = Instant::now();
                let mut readiness_complete_at: Option<Instant> = None;
                let mut animation_gate_complete = false;
                loop {
                    let current = this
                        .update(cx, |picker, cx| {
                            if picker.grid.render_version != render_version {
                                return false;
                            }
                            cx.notify();
                            true
                        })
                        .unwrap_or(false);
                    if !current {
                        stability::cancel_stationary_window();
                        let exhausted = this
                            .update(cx, |picker, cx| {
                                picker.restart_bench_sweep_after_catalog_change(cx)
                            })
                            .unwrap_or(true);
                        if exhausted {
                            crate::log_ui!(error, "bench.picker_stability failed channel={} reason=catalog_churn retries_exhausted=true", channel);
                            cx.update_window(window_handle, |_view, window, _cx| {
                                window.remove_window();
                            })
                            .ok();
                        } else {
                            crate::log_ui!(warn, "bench.picker_sweep restart channel={} reason=catalog_changed pass=stability", channel);
                        }
                        return;
                    }
                    cx.background_executor().timer(stability_tick).await;
                    if let Some(ready_at) = readiness_complete_at {
                        let continuity_elapsed_ms = ready_at.elapsed().as_millis();
                        if continuity_elapsed_ms >= BENCH_ANIMATION_CONTINUITY_MIN_MS
                            && stability::stationary_window_has_continuity(
                                BENCH_STABILITY_MINIMUM_RANGE_OBSERVATIONS,
                                BENCH_ANIMATION_CONTINUITY_MIN_TRANSITIONS,
                                BENCH_ANIMATION_CONTINUITY_MAX_STALL_MS,
                            )
                        {
                            animation_gate_complete = true;
                            break;
                        }
                        if continuity_elapsed_ms >= BENCH_ANIMATION_CONTINUITY_MAX_MS {
                            break;
                        }
                    } else if stability::stationary_window_is_stable(
                        BENCH_STABILITY_MINIMUM_RANGE_OBSERVATIONS,
                    ) {
                        if !stability::begin_continuity_phase() {
                            crate::log_ui!(error, "bench.picker_stability failed channel={} reason=continuity_phase_unavailable", channel);
                            break;
                        }
                        let ready_at = Instant::now();
                        readiness_complete_at = Some(ready_at);
                        crate::log_ui!(
                            info,
                            "bench.picker_stability readiness_complete channel={} elapsed_ms={} continuity_min_ms={} continuity_max_ms={} minimum_transitions_per_cell={} maximum_stall_ms={}",
                            channel,
                            readiness_started_at.elapsed().as_millis(),
                            BENCH_ANIMATION_CONTINUITY_MIN_MS,
                            BENCH_ANIMATION_CONTINUITY_MAX_MS,
                            BENCH_ANIMATION_CONTINUITY_MIN_TRANSITIONS,
                            BENCH_ANIMATION_CONTINUITY_MAX_STALL_MS,
                        );
                    } else if readiness_started_at.elapsed() >= stability_sample {
                        break;
                    }
                }

                let Some(snapshot) = stability::finish_stationary_window() else {
                    crate::log_ui!(error, "bench.picker_stability failed channel={} reason=missing_snapshot", channel);
                    cx.update_window(window_handle, |_view, window, _cx| {
                        window.remove_window();
                    })
                    .ok();
                    return;
                };
                log_bench_animation_diagnostics(
                    &channel,
                    rows.as_slice(),
                    visible_start,
                    visible_end,
                    &snapshot,
                );
                let animation_timeout = readiness_complete_at.is_none();
                let continuity_elapsed_ms = readiness_complete_at
                    .map(|ready_at| ready_at.elapsed().as_millis())
                    .unwrap_or(0);
                let stable = !animation_timeout
                    && animation_gate_complete
                    && snapshot.is_continuously_animating(
                        BENCH_STABILITY_MINIMUM_RANGE_OBSERVATIONS,
                        BENCH_ANIMATION_CONTINUITY_MIN_TRANSITIONS,
                        BENCH_ANIMATION_CONTINUITY_MAX_STALL_MS,
                    );
                crate::log_ui!(
                    info,
                    "bench.picker_stability complete channel={} stable={} stationary=true animation_mode={} native_animated={} animation_timeout={} continuity_elapsed_ms={} elapsed_ms={} expected_visible_cells={} expected_visible_animated_cells={} observed_unique_cells={} rendered_unique_cells={} final_placeholder_cells={} range_observations={} visible_range_changes={} rendered_to_placeholder_transitions={} post_rendered_placeholder_observations={} cell_observations={} rendered_observations={} placeholder_observations={} placeholder_to_rendered_transitions={} metadata_ready_animated_cells={} advanced_animated_cells={} animated_frame_observations={} animated_frame_transitions={} animated_source_switches={} minimum_animated_cell_transitions={} maximum_animated_cell_stall_ms={}",
                    channel,
                    stable,
                    animation_mode_name,
                    native_animated,
                    animation_timeout,
                    continuity_elapsed_ms,
                    snapshot.elapsed_ms,
                    snapshot.expected_visible_cells,
                    snapshot.expected_visible_animated_cells,
                    snapshot.unique_cells,
                    snapshot.rendered_unique_cells,
                    snapshot.final_placeholder_cells,
                    snapshot.range_observations,
                    snapshot.visible_range_changes,
                    snapshot.rendered_to_placeholder_transitions,
                    snapshot.post_rendered_placeholder_observations,
                    snapshot.cell_observations,
                    snapshot.rendered_observations,
                    snapshot.placeholder_observations,
                    snapshot.placeholder_to_rendered_transitions,
                    snapshot.metadata_ready_animated_cells,
                    snapshot.advanced_animated_cells,
                    snapshot.animated_frame_observations,
                    snapshot.animated_frame_transitions,
                    snapshot.animated_source_switches,
                    snapshot.minimum_animated_cell_transitions,
                    snapshot.maximum_animated_cell_stall_ms,
                );
                if animation_timeout {
                    crate::log_ui!(error, "bench.picker_stability failed channel={} reason=animation_timeout expected_visible_animated_cells={} metadata_ready_animated_cells={} advanced_animated_cells={} animated_frame_transitions={}", channel, snapshot.expected_visible_animated_cells, snapshot.metadata_ready_animated_cells, snapshot.advanced_animated_cells, snapshot.animated_frame_transitions);
                } else if !stable {
                    crate::log_ui!(error, "bench.picker_stability failed channel={} reason=animation_continuity expected_visible_animated_cells={} animated_source_switches={} minimum_animated_cell_transitions={} maximum_animated_cell_stall_ms={} continuity_elapsed_ms={}", channel, snapshot.expected_visible_animated_cells, snapshot.animated_source_switches, snapshot.minimum_animated_cell_transitions, snapshot.maximum_animated_cell_stall_ms, continuity_elapsed_ms);
                }
            } else {
                crate::log_ui!(error, "bench.picker_stability failed channel={} reason=incomplete_sweep", channel);
            }

            crate::log_ui!(
                info,
                "bench.picker_sweep complete channel={} coverage_complete={} maximized=true emotes={} rows={} forward_steps={} reverse_steps={}",
                channel,
                coverage_complete,
                shown,
                row_count,
                forward_steps,
                reverse_steps
            );

            cx.background_executor().timer(Duration::from_secs(2)).await;
            cx.update_window(window_handle, |_view, window, _cx| {
                window.remove_window();
            })
            .ok();
        })
        .detach();
    }
}
