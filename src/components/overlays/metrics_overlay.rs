//! Performance Metrics Dashboard Overlay
//!
//! Displays real-time performance metrics:
//! - Frame time and FPS
//! - Memory usage (cache + message buffers)
//! - Cache hit rates
//! - Message throughput
//! - Active emote animations

use crate::backend::metrics::{EmoteStageRow, METRICS};
use crate::cache::loader::stats;
use crate::theme::colors;
use crate::theme::tokens::{Radii, RadiiPixels};
use gpui::*;
use std::time::{Duration, Instant};

/// The dashboard reads global counters that the rest of the app is actively
/// mutating, so sampling it on every render measures the overlay as much as the
/// app. Resample on a coarse diagnostic cadence instead.
const DASHBOARD_SAMPLE_INTERVAL: Duration = Duration::from_millis(300);

/// State for the metrics overlay
#[derive(Clone, Debug, Default)]
pub struct MetricsOverlayState {
    pub is_open: bool,
}

impl MetricsOverlayState {
    pub fn toggle(&mut self) {
        self.is_open = !self.is_open;
    }
}

/// One formatted dashboard line, retained between samples.
#[derive(Clone)]
struct MetricRow {
    label: SharedString,
    value: SharedString,
    color: Hsla,
}

fn metric_row(
    label: impl Into<SharedString>,
    value: impl Into<SharedString>,
    color: Hsla,
) -> MetricRow {
    MetricRow {
        label: label.into(),
        value: value.into(),
        color,
    }
}

/// Immutable, fully formatted dashboard contents for one sample.
#[derive(Default)]
struct DashboardSnapshot {
    primary: Vec<MetricRow>,
    top_splits: Vec<MetricRow>,
    notify_sources: Vec<MetricRow>,
    picker_stages: Vec<MetricRow>,
    canvas_stages: Vec<MetricRow>,
}

/// Metrics overlay component
pub struct MetricsOverlay {
    pub state: MetricsOverlayState,
    /// Smoothed frame time for display
    smoothed_frame_ms: f64,
    /// Last update time
    last_update: Instant,
    /// Last sampled dashboard contents
    dashboard: DashboardSnapshot,
    /// When `dashboard` was sampled, `None` before the first sample
    last_sample: Option<Instant>,
}

impl MetricsOverlay {
    pub fn new() -> Self {
        Self {
            state: MetricsOverlayState::default(),
            smoothed_frame_ms: 0.0,
            last_update: Instant::now(),
            dashboard: DashboardSnapshot::default(),
            last_sample: None,
        }
    }

    pub fn is_open(&self) -> bool {
        self.state.is_open
    }

    pub fn toggle(&mut self) {
        self.state.toggle();
    }

    /// Update frame timing (call once per frame)
    pub fn update_frame_timing(&mut self) {
        let now = Instant::now();
        let frame_ms = now.duration_since(self.last_update).as_secs_f64() * 1000.0;
        self.last_update = now;

        const ALPHA: f64 = 0.1;
        self.smoothed_frame_ms = ALPHA * frame_ms + (1.0 - ALPHA) * self.smoothed_frame_ms;
    }

    fn dashboard_is_stale(&self, now: Instant) -> bool {
        match self.last_sample {
            Some(sampled) => now.duration_since(sampled) >= DASHBOARD_SAMPLE_INTERVAL,
            None => true,
        }
    }

    /// Read every counter source once and format the whole dashboard.
    fn sample_dashboard(&mut self, now: Instant) {
        let cache_stats = stats().snapshot();

        let (msgs_per_sec, emote_success_rate) = match METRICS.read() {
            Ok(m) => {
                let total_emotes = m.emote_loads + m.emote_failures;
                let emote_rate = if total_emotes > 0 {
                    (m.emote_loads as f64 / total_emotes as f64) * 100.0
                } else {
                    100.0
                };
                (m.messages_per_second, emote_rate)
            }
            _ => (0.0, 100.0),
        };

        let poll_cadence_fps = if self.smoothed_frame_ms > 0.0 {
            1000.0 / self.smoothed_frame_ms
        } else {
            0.0
        };

        let profiler_stats = crate::diagnostics::fps_profiler::get_stats();
        let canvas_cpu_ms = crate::diagnostics::fps_profiler::canvas_cpu_ms();
        let frame_jank_count = crate::diagnostics::fps_profiler::frame_jank_count();
        let gpu_ms = crate::diagnostics::fps_profiler::gpu_ms();
        let gpu_sample_count = crate::diagnostics::fps_profiler::gpu_sample_count();
        let present_latency_ms = crate::diagnostics::fps_profiler::present_latency_ms();
        let gpui_frame_durations = crate::diagnostics::fps_profiler::gpui_frame_duration_stats();

        let total_memory_mb = cache_stats.memory_used as f64 / (1024.0 * 1024.0);
        let frame_memory_mb = cache_stats.frame_memory as f64 / (1024.0 * 1024.0);
        let summary = crate::backend::metrics::get_summary();
        let frame_diag = crate::managers::frame_rate_controller::diagnostics();
        let eventapi_diag = crate::backend::emotes::seventv_eventapi::diagnostics();
        let layout_summary = summary.layout;
        let layout_worker = crate::components::chat::canvas_renderer::layout_worker_diagnostics();
        let (pinned_local_only, pinned_sampled) =
            crate::components::chat::canvas_renderer::global_layout_cache::pinned_layout_probe();

        let primary = vec![
            metric_row(
                "Poll cadence",
                format!(
                    "{:.1}ms ({:.0} FPS)",
                    self.smoothed_frame_ms, poll_cadence_fps
                ),
                if poll_cadence_fps >= 60.0 {
                    colors::signal_green()
                } else if poll_cadence_fps >= 30.0 {
                    colors::signal_yellow()
                } else {
                    colors::signal_red()
                },
            ),
            metric_row(
                "CPU work ms",
                if profiler_stats.work_ms > 0.0 {
                    format!(
                        "canvas {:.2} / frame {:.2}",
                        canvas_cpu_ms, profiler_stats.work_ms
                    )
                } else {
                    format!("canvas {:.2}", canvas_cpu_ms)
                },
                stage_color(canvas_cpu_ms),
            ),
            metric_row(
                "Frame jank",
                if crate::diagnostics::fps_profiler::is_enabled() {
                    format!("{}", frame_jank_count)
                } else {
                    "n/a (profiler off)".to_string()
                },
                if frame_jank_count > 0 {
                    colors::signal_yellow()
                } else {
                    colors::text_secondary()
                },
            ),
            metric_row(
                "GPU ms",
                match gpu_ms {
                    Some(ms) => format!("{:.2} (n:{})", ms, gpu_sample_count.unwrap_or(0)),
                    None => "n/a (awaiting timestamp)".to_string(),
                },
                gpu_ms.map(stage_color).unwrap_or_else(colors::text_muted),
            ),
            metric_row(
                "Present latency",
                match present_latency_ms {
                    Some(ms) => format!("{:.2}ms", ms),
                    None => "n/a (no present stats yet)".to_string(),
                },
                match present_latency_ms {
                    Some(_) => colors::text_secondary(),
                    None => colors::text_muted(),
                },
            ),
            metric_row(
                "GPUI draw (main)",
                match gpui_frame_durations {
                    Some(stats) => format!(
                        "p50 {:.2} / p95 {:.2}ms (n:{})",
                        stats.draw_p50_ms, stats.draw_p95_ms, stats.draw_samples
                    ),
                    None if !cfg!(feature = "gpui-frame-duration-histogram") => {
                        "n/a (build without GPUI histogram)".to_string()
                    }
                    None => "n/a (awaiting lifetime sample)".to_string(),
                },
                gpui_frame_durations
                    .map(|stats| stage_color(stats.draw_p95_ms))
                    .unwrap_or_else(colors::text_muted),
            ),
            metric_row(
                "GPUI anim (main)",
                match gpui_frame_durations.and_then(|stats| {
                    stats
                        .animation_present_p50_ms
                        .zip(stats.animation_present_p95_ms)
                        .map(|(p50_ms, p95_ms)| {
                            format!(
                                "p50 {:.2} / p95 {:.2}ms (n:{})",
                                p50_ms, p95_ms, stats.animation_present_samples
                            )
                        })
                }) {
                    Some(summary) => summary,
                    None if !cfg!(feature = "gpui-frame-duration-histogram") => {
                        "n/a (build without GPUI histogram)".to_string()
                    }
                    None => "n/a (no lifetime animation samples)".to_string(),
                },
                colors::text_secondary(),
            ),
            metric_row(
                "Memory",
                format!("{:.1}MB + {:.1}MB", total_memory_mb, frame_memory_mb),
                colors::text_secondary(),
            ),
            metric_row(
                "Cache Hit",
                format!("{:.1}%", cache_stats.hit_rate),
                if cache_stats.hit_rate >= 90.0 {
                    colors::signal_green()
                } else if cache_stats.hit_rate >= 70.0 {
                    colors::signal_yellow()
                } else {
                    colors::signal_red()
                },
            ),
            metric_row(
                "Layout sources",
                format!(
                    "sp:{} gl:{} as:{} sy:{}",
                    layout_summary.hit_split_cache,
                    layout_summary.hit_global_cache,
                    layout_summary.hit_async_ready,
                    layout_summary.miss_sync_shape,
                ),
                if layout_summary.miss_sync_shape > 0 {
                    colors::signal_yellow()
                } else {
                    colors::text_secondary()
                },
            ),
            metric_row(
                "Layout queue",
                format!(
                    "{}/{} pk:{} maxw:{}us",
                    layout_worker.queue_depth,
                    layout_worker.queue_capacity,
                    layout_worker.queue_peak_depth,
                    layout_worker.queue_wait_max_us,
                ),
                if layout_worker.queue_capacity > 0
                    && layout_worker.queue_depth.saturating_mul(4)
                        >= layout_worker.queue_capacity.saturating_mul(3)
                {
                    colors::signal_yellow()
                } else {
                    colors::text_secondary()
                },
            ),
            metric_row(
                "Layout jobs",
                format!(
                    "e:{} s:{} p:{}",
                    layout_worker.enqueued_total,
                    layout_worker.shaped_total,
                    layout_worker.published_total,
                ),
                colors::text_secondary(),
            ),
            metric_row(
                "Layout outcomes",
                format!(
                    "f:{} d:{} p:{} pre:{} post:{}",
                    layout_worker.queue_full_total,
                    layout_worker.queue_disconnected_total,
                    layout_worker.worker_panic_total,
                    layout_worker.obsolete_before_shape_total,
                    layout_worker.obsolete_after_shape_total,
                ),
                if layout_worker.queue_disconnected_total > 0
                    || layout_worker.worker_panic_total > 0
                {
                    colors::signal_red()
                } else {
                    colors::text_secondary()
                },
            ),
            metric_row(
                "Layout pinned",
                format!("{}/{}", pinned_local_only, pinned_sampled),
                colors::text_secondary(),
            ),
            metric_row(
                "Msgs/sec",
                format!("{:.1}", msgs_per_sec),
                colors::text_secondary(),
            ),
            metric_row(
                "Emotes",
                format!(
                    "{:.1}% ({}/{})",
                    emote_success_rate, cache_stats.static_images, cache_stats.animated_images
                ),
                colors::text_secondary(),
            ),
            metric_row(
                "Downloads",
                format!("{}", cache_stats.active_downloads),
                if cache_stats.active_downloads > 10 {
                    colors::signal_yellow()
                } else {
                    colors::text_secondary()
                },
            ),
            metric_row(
                "Fresh-hold",
                format!("{}", cache_stats.freshness_blocked_evictions),
                colors::text_secondary(),
            ),
            metric_row(
                "Mode",
                format!("{:?}", frame_diag.mode),
                colors::text_secondary(),
            ),
            metric_row(
                "IRC age",
                format_age_ms(frame_diag.last_irc_message_ms_ago),
                colors::text_secondary(),
            ),
            metric_row(
                "Image age",
                format_age_ms(frame_diag.last_image_ready_ms_ago),
                colors::text_secondary(),
            ),
            metric_row(
                "7TV EventAPI",
                format!(
                    "{} d:{} s:{} q:{}",
                    if eventapi_diag.connected {
                        "up"
                    } else {
                        "down"
                    },
                    eventapi_diag.desired_subscriptions,
                    eventapi_diag.active_subscriptions,
                    eventapi_diag.queued_subscription_commands
                ),
                if eventapi_diag.connected {
                    colors::signal_green()
                } else {
                    colors::signal_yellow()
                },
            ),
            metric_row(
                "UI notifies",
                format!("{}", summary.ui_notify_count),
                colors::text_secondary(),
            ),
            metric_row(
                "App renders",
                format!("{}", summary.app_render_count),
                colors::text_secondary(),
            ),
            metric_row(
                "Render from notify",
                format!("{}", summary.app_render_from_notify_count),
                colors::text_secondary(),
            ),
            metric_row(
                "Render external",
                format!("{}", summary.app_render_external_count),
                colors::text_secondary(),
            ),
        ];

        let top_splits = crate::backend::metrics::top_split_metrics(3)
            .into_iter()
            .map(|row| {
                let label = row
                    .split
                    .split(':')
                    .next()
                    .unwrap_or(row.split.as_str())
                    .to_string();
                metric_row(
                    label,
                    format!(
                        "r:{} u:{} dm:{}",
                        row.renders, row.updates, row.messages_delta
                    ),
                    colors::text_secondary(),
                )
            })
            .collect::<Vec<_>>();

        let notify_sources = crate::backend::metrics::top_notify_sources(3)
            .into_iter()
            .map(|row| {
                metric_row(
                    row.source,
                    format!("{}", row.count),
                    colors::text_secondary(),
                )
            })
            .collect::<Vec<_>>();

        let emote_stages = crate::backend::metrics::top_emote_stage_metrics(128);
        let canvas_stages = emote_stages
            .iter()
            .filter(|row| row.stage.starts_with("canvas."))
            .take(8)
            .map(|row| {
                let label = row
                    .stage
                    .strip_prefix("canvas.")
                    .unwrap_or(row.stage.as_str());
                let (value, color) = format_stage_row(Some(row));
                metric_row(label.to_string(), value, color)
            })
            .collect::<Vec<_>>();
        let inline_rebuild = format_stage_row(
            emote_stages
                .iter()
                .find(|row| row.stage == "picker.inline.rebuild_virtual_rows"),
        );
        let popout_rebuild = format_stage_row(
            emote_stages
                .iter()
                .find(|row| row.stage == "picker.popout.rebuild_virtual_rows"),
        );
        let picker_stages = vec![
            metric_row("Inline rebuild", inline_rebuild.0, inline_rebuild.1),
            metric_row("Popout rebuild", popout_rebuild.0, popout_rebuild.1),
        ];

        self.dashboard = DashboardSnapshot {
            primary,
            top_splits,
            notify_sources,
            picker_stages,
            canvas_stages,
        };
        self.last_sample = Some(now);
    }
}

impl Default for MetricsOverlay {
    fn default() -> Self {
        Self::new()
    }
}

impl Render for MetricsOverlay {
    fn render(&mut self, window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        if !self.state.is_open {
            return div().into_any_element();
        }

        self.update_frame_timing();

        let now = Instant::now();
        if self.dashboard_is_stale(now) {
            self.sample_dashboard(now);
        }
        let dashboard = &self.dashboard;

        let viewport = window.viewport_size();
        let fit =
            crate::utils::fit_overlay_to_viewport(viewport, 300.0, f32::from(viewport.height), 8.0);

        div()
            .id("metrics-overlay")
            .absolute()
            .top(px(fit.margin_y))
            .right(px(fit.margin_x))
            .w(px(fit.width))
            .max_h(px(fit.height))
            .p_2()
            .bg(colors::bg_elevated().opacity(0.9))
            .rounded(Radii::md())
            .border_1()
            .border_color(colors::border())
            .shadow_md()
            .flex()
            .flex_col()
            .gap_1()
            .overflow_y_scroll()
            .overflow_x_hidden()
            .child(
                div()
                    .text_xs()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(colors::text_primary())
                    .child("Performance"),
            )
            .child(div().h(px(1.0)).bg(colors::border()))
            .children(
                dashboard
                    .primary
                    .iter()
                    .map(|row| render_metric_row(row).into_any_element()),
            )
            .child(div().h(px(1.0)).bg(colors::border()))
            .child(
                div()
                    .text_xs()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(colors::text_primary())
                    .child("Top Splits"),
            )
            .children(
                dashboard
                    .top_splits
                    .iter()
                    .map(|row| render_metric_row(row).into_any_element()),
            )
            .child(div().h(px(1.0)).bg(colors::border()))
            .child(
                div()
                    .text_xs()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(colors::text_primary())
                    .child("Notify Sources"),
            )
            .children(
                dashboard
                    .notify_sources
                    .iter()
                    .map(|row| render_metric_row(row).into_any_element()),
            )
            .child(div().h(px(1.0)).bg(colors::border()))
            .child(
                div()
                    .text_xs()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(colors::text_primary())
                    .child("Picker Stages"),
            )
            .children(
                dashboard
                    .picker_stages
                    .iter()
                    .map(|row| render_metric_row(row).into_any_element()),
            )
            .child(div().h(px(1.0)).bg(colors::border()))
            .child(
                div()
                    .text_xs()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(colors::text_primary())
                    .child("Canvas Stages"),
            )
            .children(
                dashboard
                    .canvas_stages
                    .iter()
                    .map(|row| render_metric_row(row).into_any_element()),
            )
            .into_any_element()
    }
}

/// Render a single metric row
fn render_metric_row(row: &MetricRow) -> Div {
    div()
        .w_full()
        .flex()
        .items_center()
        .justify_between()
        .child(
            div()
                .text_xs()
                .text_color(colors::text_muted())
                .child(row.label.clone()),
        )
        .child(
            div()
                .text_xs()
                .font_weight(FontWeight::MEDIUM)
                .text_color(row.color)
                .child(row.value.clone()),
        )
}

fn stage_color(avg_ms: f64) -> Hsla {
    if avg_ms < 1.0 {
        colors::signal_green()
    } else if avg_ms < 3.5 {
        colors::signal_yellow()
    } else {
        colors::signal_red()
    }
}

fn format_age_ms(value: Option<u64>) -> String {
    match value {
        Some(ms) if ms < 1_000 => format!("{}ms", ms),
        Some(ms) => format!("{:.1}s", ms as f64 / 1_000.0),
        None => "n/a".to_string(),
    }
}

fn format_stage_row(row: Option<&EmoteStageRow>) -> (SharedString, Hsla) {
    if let Some(row) = row {
        (
            SharedString::from(format!(
                "avg:{:.2}ms last:{:.2}ms n:{}",
                row.avg_ms, row.last_ms, row.count
            )),
            stage_color(row.avg_ms),
        )
    } else {
        (SharedString::from("no samples"), colors::text_muted())
    }
}
