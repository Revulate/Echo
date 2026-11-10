//! FPS Profiler - Real-time frame rate analysis and performance monitoring
//!
//! Provides comprehensive frame timing analysis for GPUI applications:
//! - Rolling frame time history with percentile calculations (p50, p95, p99)
//! - Frame breakdown by phase (layout, paint, etc.)
//! - Jank detection (frames exceeding target frame time)
//! - Real-time FPS overlay rendering
//! - ZED_MEASUREMENTS style output for performance comparison
//!
//! ## Usage
//!
//! ```rust,no_run
//! use echo_gpui::diagnostics::fps_profiler;
//! use std::time::Duration;
//!
//! // At start of frame
//! fps_profiler::begin_frame();
//!
//! // Record phase timings
//! let layout_duration = Duration::from_millis(4);
//! let paint_duration = Duration::from_millis(2);
//! fps_profiler::record_phase("layout", layout_duration);
//! fps_profiler::record_phase("paint", paint_duration);
//!
//! // At end of frame
//! fps_profiler::end_frame();
//!
//! // Get statistics
//! let stats = fps_profiler::get_stats();
//! println!("FPS: {:.1} (p95 frame: {:.2}ms)", stats.fps, stats.p95_ms);
//! ```
//!
//! ## Environment Variables
//!
//! - `ECHO_FPS_PROFILER=1` - Enable frame profiling and phase timing collection
//! - `ECHO_FPS_OVERLAY=1` - Show FPS overlay in top-left corner
//! - `ECHO_MEASUREMENTS=1` - Output ZED-style frame timing to stderr
//! - `ECHO_JANK_THRESHOLD=20` - Set jank threshold in ms (default: 16.67)

use crate::backend::lock_utils::RwLockExt;
use std::collections::VecDeque;
use std::sync::RwLock;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// Number of frames to keep in history for statistics
const FRAME_HISTORY_SIZE: usize = 300;

/// Default jank threshold in milliseconds (frames > this are considered jank)
const DEFAULT_JANK_THRESHOLD_MS: f64 = 16.67;

/// Cadence-independent jank gap floor. The effective threshold is twice the
/// current animation-cap tick interval (never below this floor): jank means a
/// whole scheduled slot was missed. Anything tighter double-counts Windows
/// timer coalescing — the ~15.6ms quantum makes healthy 50Hz ticks land in a
/// bimodal ~15/30ms pattern, which a tick+small-margin threshold scored as
/// ~27% dropped frames on an idle channel. This is distinct from the
/// cadence-vs-16.67ms `jank_percent`, which compares sub-ms element-build
/// time against the frame budget and so under-reports real dropped frames.
const FRAME_GAP_JANK_THRESHOLD_MS: f64 = 20.0;

/// Maximum phases per frame
const MAX_PHASES: usize = 16;

/// Global frame profiler state
static PROFILER: RwLock<FrameProfiler> = RwLock::new(FrameProfiler::new_const());

/// Counter for unique frame IDs
static FRAME_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Whether overlay display is enabled
static OVERLAY_ENABLED: AtomicBool = AtomicBool::new(false);

/// Whether frame profiler collection is enabled.
static PROFILER_ENABLED: AtomicBool = AtomicBool::new(false);

/// Whether the full metrics dashboard is open. This source is separate from
/// `ECHO_FPS_OVERLAY`, which controls the compact top-left overlay.
static DASHBOARD_ENABLED: AtomicBool = AtomicBool::new(false);

/// Whether measurements output is enabled (ZED_MEASUREMENTS style)
static MEASUREMENTS_ENABLED: AtomicBool = AtomicBool::new(false);

/// Real CPU paint-prep cost of the canvas path (`CanvasMessageList::render`), in nanoseconds.
///
/// This is recorded directly by the renderer via [`record_canvas_cpu_nanos`] and is
/// **independent** of poll cadence and of whether the frame profiler is enabled — it
/// always reflects the most recent canvas render so the metrics overlay can surface the
/// honest CPU cost even in normal (profiler-disabled) runs.
static CANVAS_CPU_NANOS: AtomicU64 = AtomicU64::new(0);

/// Real GPU paint-emit cost of the canvas viewport element (`ChatViewportElement::paint`),
/// in nanoseconds.
///
/// This is recorded directly by the viewport element via [`record_canvas_paint_nanos`] and
/// is **independent** of [`CANVAS_CPU_NANOS`] above: that static measures paint-*prep* (the
/// `render()` body that runs before GPUI returns the element tree), whereas this static
/// measures the actual paint pass that GPUI invokes afterward. Surfaced as the `paint_emit`
/// phase in the breakdown log.
static CANVAS_PAINT_NANOS: AtomicU64 = AtomicU64::new(0);

/// Cadence-independent dropped-frame count: number of render starts that arrived more
/// than [`FRAME_GAP_JANK_THRESHOLD_MS`] after the previous render's end.
static FRAME_JANK_COUNT: AtomicU64 = AtomicU64::new(0);

/// Per-class frame counters, bumped once per canvas render frame by the renderer via
/// [`record_frame_classification`]. These mirror the per-split `RenderPerfFrameStats`
/// reuse/classification fields but are global (cross-split, cumulative) so the 30-frame
/// breakdown log can report, of recent frames, how many took each fast/slow path.
static FRAME_CLASS_STABLE_REUSE: AtomicU64 = AtomicU64::new(0);
static FRAME_CLASS_SCROLL_TRANSLATE: AtomicU64 = AtomicU64::new(0);
static FRAME_CLASS_FULL_REBUILD: AtomicU64 = AtomicU64::new(0);
static FRAME_CLASS_ANIMATION_ONLY: AtomicU64 = AtomicU64::new(0);

/// Most recent present (swap-chain) latency in milliseconds, stored as `f64::to_bits`.
///
/// Recorded each main-window draw via [`record_main_window_renderer_statistics`], which
/// reads GPUI's `Window::present_statistics` (a local `echo-paint-fork` extension mapping
/// `IDXGISwapChain::GetFrameStatistics`). The sentinel [`PRESENT_LATENCY_NONE`] means no
/// honest reading is available (non-Windows, non-flip-model swap chain, statistics
/// disjoint, or before the first vsync-aligned present) — never a fabricated number.
static PRESENT_LATENCY_BITS: AtomicU64 = AtomicU64::new(PRESENT_LATENCY_NONE);

/// Sentinel stored in [`PRESENT_LATENCY_BITS`] meaning "no present latency reading".
/// `u64::MAX` is not a valid `f64::to_bits` value of any finite/zero latency we record.
const PRESENT_LATENCY_NONE: u64 = u64::MAX;

/// Running swap-chain present count from the most recent reading (0 until first sample).
static PRESENT_COUNT: AtomicU64 = AtomicU64::new(0);

/// Most recently resolved GPU frame duration, stored as `f64::to_bits`.
///
/// GPUI's local Windows Direct3D and supported native Linux wgpu backends resolve a
/// bounded timestamp-query ring on later frames without flushing or blocking.
/// [`GPU_FRAME_MS_NONE`] means diagnostics are disabled, the first query has not
/// completed, or the backend or adapter is unsupported.
static GPU_FRAME_MS_BITS: AtomicU64 = AtomicU64::new(GPU_FRAME_MS_NONE);

/// Sentinel stored in [`GPU_FRAME_MS_BITS`] meaning "no GPU timestamp result".
const GPU_FRAME_MS_NONE: u64 = u64::MAX;

/// Running count from the most recently resolved GPUI GPU timestamp sample.
static GPU_FRAME_SAMPLE_COUNT: AtomicU64 = AtomicU64::new(0);

/// GPUI's upstream histogram snapshot is intentionally sampled only while diagnostics are
/// active. Cloning an HDR histogram every paint would distort the very work being observed, so
/// the live window is sampled at most twice a second and only the summaries required by the
/// diagnostic UI are retained. The upstream per-frame recorder itself exists only in builds made
/// with Echo's `gpui-frame-duration-histogram` feature.
#[cfg(feature = "gpui-frame-duration-histogram")]
const GPUI_FRAME_DURATION_SNAPSHOT_INTERVAL_MS: u64 = 500;
#[cfg(feature = "gpui-frame-duration-histogram")]
static GPUI_FRAME_DURATION_LAST_SNAPSHOT_MS: AtomicU64 = AtomicU64::new(0);
#[cfg(feature = "gpui-frame-duration-histogram")]
static GPUI_FRAME_DURATION_SNAPSHOT_EPOCH: std::sync::LazyLock<Instant> =
    std::sync::LazyLock::new(Instant::now);

/// Count of `Window::draw` durations in the most recently sampled GPUI histogram.
static GPUI_DRAW_DURATION_SAMPLE_COUNT: AtomicU64 = AtomicU64::new(0);
/// Draw-duration p50 and p95 in milliseconds, stored as `f64::to_bits`.
static GPUI_DRAW_DURATION_P50_MS_BITS: AtomicU64 = AtomicU64::new(0);
static GPUI_DRAW_DURATION_P95_MS_BITS: AtomicU64 = AtomicU64::new(0);
/// Count of active-animation present intervals in the most recently sampled GPUI histogram.
static GPUI_PRESENT_INTERVAL_SAMPLE_COUNT: AtomicU64 = AtomicU64::new(0);
/// Active-animation present-interval p50 and p95 in milliseconds, stored as `f64::to_bits`.
static GPUI_PRESENT_INTERVAL_P50_MS_BITS: AtomicU64 = AtomicU64::new(0);
static GPUI_PRESENT_INTERVAL_P95_MS_BITS: AtomicU64 = AtomicU64::new(0);

/// Timing data for a single phase within a frame
#[derive(Clone, Debug)]
pub struct PhaseTimingRaw {
    /// Phase name (e.g., "layout", "paint", "emotes")
    pub name: &'static str,
    /// Duration of this phase
    pub duration: Duration,
}

/// Complete timing data for a single frame
#[derive(Clone, Debug)]
pub struct FrameTiming {
    /// Unique frame identifier
    pub frame_id: u64,
    /// Total frame duration
    pub total_duration: Duration,
    /// Whether this frame was considered "jank"
    pub is_jank: bool,
}

impl FrameTiming {
    /// Get total frame time in milliseconds
    pub fn total_ms(&self) -> f64 {
        self.total_duration.as_secs_f64() * 1000.0
    }
}

/// Active frame being recorded
struct ActiveFrame {
    frame_id: u64,
    start_time: Instant,
    phases: Vec<PhaseTimingRaw>,
}

impl ActiveFrame {
    fn new(frame_id: u64) -> Self {
        Self {
            frame_id,
            start_time: Instant::now(),
            phases: Vec::with_capacity(MAX_PHASES),
        }
    }
}

/// Main frame profiler state
struct FrameProfiler {
    /// History of frame timings (ring buffer)
    frame_history: VecDeque<FrameTiming>,
    /// History of wall-clock frame intervals (ms) between frame begins (poll cadence)
    interval_history_ms: VecDeque<f64>,
    /// History of actual CPU work durations (ms) per frame (`FrameTiming::total_duration`).
    /// This is the element-tree build cost, separate from the cadence above.
    work_history_ms: VecDeque<f64>,
    /// Currently active frame (if any)
    active_frame: Option<ActiveFrame>,
    /// Timestamp of previous frame begin (for cadence FPS)
    last_frame_begin: Option<Instant>,
    /// Timestamp of previous frame end (for cadence-independent jank gap detection)
    last_frame_end: Option<Instant>,
    /// Jank threshold in milliseconds
    jank_threshold_ms: f64,
    /// Profiler start time (for session stats)
    start_time: Option<Instant>,
    /// Total frames recorded
    total_frames: u64,
    /// Total jank frames
    jank_frames: u64,
    /// Aggregated phase stats (phase_name -> (total_ms, count))
    phase_stats: Vec<(&'static str, f64, u64)>,
}

impl FrameProfiler {
    /// Create a new profiler (const fn for static initialization)
    const fn new_const() -> Self {
        Self {
            frame_history: VecDeque::new(),
            interval_history_ms: VecDeque::new(),
            work_history_ms: VecDeque::new(),
            active_frame: None,
            last_frame_begin: None,
            last_frame_end: None,
            jank_threshold_ms: DEFAULT_JANK_THRESHOLD_MS,
            start_time: None,
            total_frames: 0,
            jank_frames: 0,
            phase_stats: Vec::new(),
        }
    }

    /// Initialize with capacity
    fn init(&mut self) {
        if self.start_time.is_none() {
            self.start_time = Some(Instant::now());
            self.frame_history = VecDeque::with_capacity(FRAME_HISTORY_SIZE);
            self.interval_history_ms = VecDeque::with_capacity(FRAME_HISTORY_SIZE);
            self.work_history_ms = VecDeque::with_capacity(FRAME_HISTORY_SIZE);
            self.phase_stats = Vec::with_capacity(MAX_PHASES);
        }
    }

    /// Begin a new frame
    fn begin_frame(&mut self) -> u64 {
        self.init();
        let now = Instant::now();
        if let Some(prev) = self.last_frame_begin {
            let interval_ms = now.duration_since(prev).as_secs_f64() * 1000.0;
            if interval_ms > 0.0 && interval_ms < 1000.0 {
                self.interval_history_ms.push_back(interval_ms);
                if self.interval_history_ms.len() > FRAME_HISTORY_SIZE {
                    self.interval_history_ms.pop_front();
                }
            }
        }
        if let Some(prev_end) = self.last_frame_end {
            let gap_ms = now.duration_since(prev_end).as_secs_f64() * 1000.0;
            let jank_gap_threshold_ms =
                (crate::managers::dynamic_tick_scheduler::animation_min_tick_interval_ms() as f64
                    * 2.0)
                    .max(FRAME_GAP_JANK_THRESHOLD_MS);
            if gap_ms > jank_gap_threshold_ms {
                FRAME_JANK_COUNT.fetch_add(1, Ordering::Relaxed);
            }
        }
        self.last_frame_begin = Some(now);
        let frame_id = FRAME_COUNTER.fetch_add(1, Ordering::Relaxed);
        self.active_frame = Some(ActiveFrame::new(frame_id));
        frame_id
    }

    /// Record a phase timing for the current frame
    fn record_phase(&mut self, name: &'static str, duration: Duration) {
        if let Some(ref mut frame) = self.active_frame {
            frame.phases.push(PhaseTimingRaw { name, duration });
        }
    }

    /// End the current frame and record its timing
    fn end_frame(&mut self) -> Option<FrameTiming> {
        let active = self.active_frame.take()?;
        let total_duration = active.start_time.elapsed();
        let total_ms = total_duration.as_secs_f64() * 1000.0;
        let is_jank = total_ms > self.jank_threshold_ms;

        for phase in &active.phases {
            let phase_ms = phase.duration.as_secs_f64() * 1000.0;
            let found = self
                .phase_stats
                .iter_mut()
                .find(|(n, _, _)| *n == phase.name);
            if let Some((_, total, count)) = found {
                *total += phase_ms;
                *count += 1;
            } else {
                self.phase_stats.push((phase.name, phase_ms, 1));
            }
        }

        let timing = FrameTiming {
            frame_id: active.frame_id,
            total_duration,
            is_jank,
        };

        self.frame_history.push_back(timing.clone());
        if self.frame_history.len() > FRAME_HISTORY_SIZE {
            self.frame_history.pop_front();
        }

        self.work_history_ms.push_back(total_ms);
        if self.work_history_ms.len() > FRAME_HISTORY_SIZE {
            self.work_history_ms.pop_front();
        }

        self.last_frame_end = Some(Instant::now());

        self.total_frames += 1;
        if is_jank {
            self.jank_frames += 1;
        }

        if MEASUREMENTS_ENABLED.load(Ordering::Relaxed) {
            crate::log_perf!(
                info,
                "[frame {}] {:.2}ms{}",
                timing.frame_id,
                total_ms,
                if is_jank { " [JANK]" } else { "" }
            );
        }

        if self.total_frames == 1 {
            crate::log_perf!(info, "Profiler active - first frame recorded");
        }

        if self.total_frames.is_multiple_of(30) {
            let stats = self.get_stats();
            let phases: String = stats
                .phase_breakdown
                .iter()
                .map(|p| format!("{}:{:.1}ms", p.name, p.avg_ms))
                .collect::<Vec<_>>()
                .join(" ");
            let canvas_paint_ms = CANVAS_PAINT_NANOS.load(Ordering::Relaxed) as f64 / 1_000_000.0;
            let gpu_frame_ms = gpu_ms().unwrap_or(f64::NAN);
            let gpu_samples = gpu_sample_count().unwrap_or(0);
            let present_latency_ms = present_latency_ms().unwrap_or(f64::NAN);
            let present_count = present_count().unwrap_or(0);
            let class_stable = FRAME_CLASS_STABLE_REUSE.load(Ordering::Relaxed);
            let class_scroll = FRAME_CLASS_SCROLL_TRANSLATE.load(Ordering::Relaxed);
            let class_rebuild = FRAME_CLASS_FULL_REBUILD.load(Ordering::Relaxed);
            let class_anim = FRAME_CLASS_ANIMATION_ONLY.load(Ordering::Relaxed);
            let frame_class = format!(
                "stable:{} scroll:{} rebuild:{} anim:{}",
                class_stable, class_scroll, class_rebuild, class_anim
            );
            crate::log_perf!(
                info,
                cadence_fps = format!("{:.1}", stats.fps),
                work_ms = format!("{:.3}", stats.work_ms),
                canvas_cpu_ms = format!("{:.3}", stats.canvas_cpu_ms),
                canvas_paint_ms = format!("{:.3}", canvas_paint_ms),
                gpu_ms = format!("{:.3}", gpu_frame_ms),
                gpu_samples,
                present_latency_ms = format!("{:.3}", present_latency_ms),
                present_count,
                p95_ms = format!("{:.2}", stats.p95_ms),
                p99_ms = format!("{:.2}", stats.p99_ms),
                jank_pct = format!("{:.1}", stats.jank_percent),
                frame_jank_count = stats.frame_jank_count,
                frame_class = %frame_class,
                phases = %phases,
                "cadence {:.0}fps | CPU work {:.3}ms | canvas_cpu {:.3}ms | paint_emit {:.3}ms | GPU {:.3}ms (n:{}) | present {:.3}ms (n:{}) | p95 {:.2}ms | jank {:.1}% (gaps>20ms: {}) | frames[{}] | {}",
                stats.fps,
                stats.work_ms,
                stats.canvas_cpu_ms,
                canvas_paint_ms,
                gpu_frame_ms,
                gpu_samples,
                present_latency_ms,
                present_count,
                stats.p95_ms,
                stats.jank_percent,
                stats.frame_jank_count,
                frame_class,
                phases
            );
        }

        Some(timing)
    }
}

/// Interval between "GPUI frame stages" log lines.
const FRAME_STAGE_LOG_INTERVAL: Duration = Duration::from_secs(2);

/// Window id of the main window, recorded by its root view (`u64::MAX` until then).
static MAIN_WINDOW_ID: AtomicU64 = AtomicU64::new(u64::MAX);

/// Main-window frame-stage counters as of the previous log line, and when it was written.
static FRAME_STAGE_LOG: std::sync::Mutex<Option<(gpui::FrameStageStatistics, Instant)>> =
    std::sync::Mutex::new(None);

/// Log the main window's GPUI frame-stage costs at most every
/// [`FRAME_STAGE_LOG_INTERVAL`].
///
/// Called on main-window draws and by animation wakers, since animation frames
/// patched into the presented scene reach the screen without any draw.
pub fn sample_frame_stages(window: &gpui::Window) {
    if !is_enabled()
        || window.window_handle().window_id().as_u64() != MAIN_WINDOW_ID.load(Ordering::Relaxed)
    {
        return;
    }
    let Ok(mut logged) = FRAME_STAGE_LOG.try_lock() else {
        return;
    };
    let now = Instant::now();
    if logged
        .as_ref()
        .is_some_and(|(_, at)| now.duration_since(*at) < FRAME_STAGE_LOG_INTERVAL)
    {
        return;
    }
    let current = window.frame_stage_statistics();
    let previous = logged.replace((current, now));
    if let Some(delta) =
        previous.and_then(|(previous, _)| FrameStageDelta::between(&previous, &current))
    {
        log_frame_stage_delta(&delta);
    }
}

/// Write one "GPUI frame stages" line.
fn log_frame_stage_delta(delta: &FrameStageDelta) {
    {
        let renderer = delta.renderer.unwrap_or_default();
        crate::log_perf!(
            info,
            gpui_frames = delta.frames,
            gpui_draw_ms = format!("{:.3}", delta.draw_ms),
            gpui_layout_paint_ms = format!("{:.3}", delta.layout_paint_ms),
            gpui_request_layout_ms = format!("{:.3}", delta.request_layout_ms),
            gpui_prepaint_ms = format!("{:.3}", delta.prepaint_ms),
            gpui_paint_ms = format!("{:.3}", delta.paint_ms),
            gpui_views_rendered = format!("{:.1}", delta.views_rendered),
            gpui_views_reused = format!("{:.1}", delta.views_reused),
            gpui_scene_finish_ms = format!("{:.3}", delta.scene_finish_ms),
            gpui_present_ms = format!("{:.3}", delta.present_ms),
            gpui_primitives = format!("{:.0}", delta.primitives),
            gpui_replayed_pct = format!("{:.1}", delta.replayed_pct),
            renderer_upload_ms = format!("{:.3}", renderer.upload_ms),
            renderer_encode_ms = format!("{:.3}", renderer.encode_ms),
            renderer_present_call_ms = format!("{:.3}", renderer.present_call_ms),
            renderer_upload_kb = format!("{:.1}", renderer.upload_kb),
            gpui_presents = delta.presents,
            gpui_partial_presents = delta.partial_presents,
            gpui_animation_patches = delta.animation_patches,
            gpui_patch_rejections = %format!(
                "no_slot:{} ambiguous:{} unresolved:{} atlas:{} drawing:{}",
                delta.patch_rejections[0],
                delta.patch_rejections[1],
                delta.patch_rejections[2],
                delta.patch_rejections[3],
                delta.patch_rejections[4]
            ),
            "GPUI frame stages: {} presents ({} partial, {} animation patches) | per drawn frame over {} frames | draw {:.3}ms (layout+paint {:.3}ms = render/request {:.3}ms + prepaint {:.3}ms + paint {:.3}ms; views {:.1} rendered {:.1} reused; scene finish {:.3}ms) | present {:.3}ms (upload {:.3}ms {:.1}KB, encode {:.3}ms, present call {:.3}ms) | primitives {:.0} ({:.1}% replayed)",
            delta.presents,
            delta.partial_presents,
            delta.animation_patches,
            delta.frames,
            delta.draw_ms,
            delta.layout_paint_ms,
            delta.request_layout_ms,
            delta.prepaint_ms,
            delta.paint_ms,
            delta.views_rendered,
            delta.views_reused,
            delta.scene_finish_ms,
            delta.present_ms,
            renderer.upload_ms,
            renderer.upload_kb,
            renderer.encode_ms,
            renderer.present_call_ms,
            delta.primitives,
            delta.replayed_pct
        );
    }
}

impl FrameProfiler {
    /// Calculate statistics from frame history
    fn get_stats(&self) -> FrameStats {
        if self.frame_history.is_empty() {
            return FrameStats::default();
        }

        let mut frame_times: Vec<f64> = self.frame_history.iter().map(|f| f.total_ms()).collect();
        frame_times.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

        let count = frame_times.len();
        let avg_ms = frame_times.iter().sum::<f64>() / count as f64;
        let min_ms = frame_times.first().copied().unwrap_or(0.0);
        let max_ms = frame_times.last().copied().unwrap_or(0.0);

        let p50_ms = percentile(&frame_times, 0.50);
        let p95_ms = percentile(&frame_times, 0.95);
        let p99_ms = percentile(&frame_times, 0.99);

        let fps = if self.interval_history_ms.is_empty() {
            if avg_ms > 0.0 { 1000.0 / avg_ms } else { 0.0 }
        } else {
            let avg_interval_ms = self.interval_history_ms.iter().sum::<f64>()
                / self.interval_history_ms.len() as f64;
            if avg_interval_ms > 0.0 {
                1000.0 / avg_interval_ms
            } else {
                0.0
            }
        };

        let recent_jank = self.frame_history.iter().filter(|f| f.is_jank).count();
        let jank_percent = (recent_jank as f64 / count as f64) * 100.0;

        let work_ms = if self.work_history_ms.is_empty() {
            avg_ms
        } else {
            self.work_history_ms.iter().sum::<f64>() / self.work_history_ms.len() as f64
        };

        let frame_jank_count = FRAME_JANK_COUNT.load(Ordering::Relaxed);
        let canvas_cpu_ms = CANVAS_CPU_NANOS.load(Ordering::Relaxed) as f64 / 1_000_000.0;
        let allocation_stats = crate::diagnostics::alloc_profile::last_frame_stats();

        let phase_breakdown = self.get_phase_breakdown();

        let variance = if count > 1 {
            let mean = avg_ms;
            let sum_sq: f64 = frame_times.iter().map(|t| (t - mean).powi(2)).sum();
            (sum_sq / (count - 1) as f64).sqrt()
        } else {
            0.0
        };

        FrameStats {
            fps,
            avg_ms,
            work_ms,
            canvas_cpu_ms,
            allocation_stats,
            min_ms,
            max_ms,
            p50_ms,
            p95_ms,
            p99_ms,
            jank_percent,
            frame_jank_count,
            variance_ms: variance,
            frame_count: count as u64,
            total_frames: self.total_frames,
            total_jank_frames: self.jank_frames,
            phase_breakdown,
        }
    }

    /// Get average timing breakdown by phase (from all recorded frames)
    fn get_phase_breakdown(&self) -> Vec<PhaseTiming> {
        self.phase_stats
            .iter()
            .map(|(name, total_ms, count)| PhaseTiming {
                name,
                avg_ms: if *count > 0 {
                    *total_ms / *count as f64
                } else {
                    0.0
                },
            })
            .collect()
    }
}

/// Calculate percentile from sorted slice
fn percentile(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let idx = ((sorted.len() as f64 - 1.0) * p).round() as usize;
    sorted[idx.min(sorted.len() - 1)]
}

/// Aggregated phase timing statistics
#[derive(Clone, Debug)]
pub struct PhaseTiming {
    /// Phase name
    pub name: &'static str,
    /// Average time for this phase
    pub avg_ms: f64,
}

/// Complete frame statistics
#[derive(Clone, Debug, Default)]
pub struct FrameStats {
    /// Current FPS (from poll cadence — wall-clock gap between frame begins, NOT render
    /// throughput). Floored by the frame-rate controller, so this is "Poll cadence FPS".
    pub fps: f64,
    /// Average frame time in milliseconds (element-tree build duration)
    pub avg_ms: f64,
    /// Average real CPU work per frame in milliseconds (element-tree build cost,
    /// independent of poll cadence). Surfaced as "CPU work ms" in the overlay.
    pub work_ms: f64,
    /// Honest canvas paint-prep CPU cost in milliseconds, recorded directly by the
    /// renderer (`CanvasMessageList::render`). Independent of profiler enable state.
    pub canvas_cpu_ms: f64,
    /// Most recent render-frame allocation counters, populated only when
    /// `ECHO_ALLOC_PROFILE=1` is set.
    pub allocation_stats: crate::diagnostics::alloc_profile::AllocationFrameStats,
    /// Minimum frame time
    pub min_ms: f64,
    /// Maximum frame time
    pub max_ms: f64,
    /// 50th percentile (median) frame time
    pub p50_ms: f64,
    /// 95th percentile frame time
    pub p95_ms: f64,
    /// 99th percentile frame time
    pub p99_ms: f64,
    /// Percentage of frames that exceeded jank threshold (cadence-vs-16.67ms; under-reports)
    pub jank_percent: f64,
    /// Cadence-independent dropped-frame count: render-start gaps > 20ms since the previous
    /// render's end (cumulative since profiler start).
    pub frame_jank_count: u64,
    /// Frame time standard deviation (stability indicator)
    pub variance_ms: f64,
    /// Number of frames in current window
    pub frame_count: u64,
    /// Total frames since profiler started
    pub total_frames: u64,
    /// Total jank frames since profiler started
    pub total_jank_frames: u64,
    /// Timing breakdown by phase
    pub phase_breakdown: Vec<PhaseTiming>,
}

impl FrameStats {
    /// Format as detailed multi-line report
    pub fn detailed_report(&self) -> String {
        let mut report = format!(
            "╔══════════════════════════════════════╗\n\
             ║  Frame Performance Report            ║\n\
             ╠══════════════════════════════════════╣\n\
             ║  FPS(cad):  {:>8.1}                 ║\n\
             ║  Avg Frame: {:>8.2} ms              ║\n\
             ║  Min Frame: {:>8.2} ms              ║\n\
             ║  Max Frame: {:>8.2} ms              ║\n\
             ╠══════════════════════════════════════╣\n\
             ║  Percentiles                         ║\n\
             ║  p50:       {:>8.2} ms              ║\n\
             ║  p95:       {:>8.2} ms              ║\n\
             ║  p99:       {:>8.2} ms              ║\n\
             ╠══════════════════════════════════════╣\n\
             ║  Jank:      {:>8.1}%                ║\n\
             ║  Variance:  {:>8.2} ms              ║\n\
             ║  Frames:    {:>8}                  ║\n",
            self.fps,
            self.avg_ms,
            self.min_ms,
            self.max_ms,
            self.p50_ms,
            self.p95_ms,
            self.p99_ms,
            self.jank_percent,
            self.variance_ms,
            self.total_frames
        );

        if !self.phase_breakdown.is_empty() {
            report.push_str("╠══════════════════════════════════════╣\n");
            report.push_str("║  Phase Breakdown                     ║\n");
            for phase in &self.phase_breakdown {
                report.push_str(&format!(
                    "║  {:<12} {:>8.2} ms            ║\n",
                    phase.name, phase.avg_ms
                ));
            }
        }

        report.push_str("╚══════════════════════════════════════╝");
        report
    }
}

impl serde::Serialize for FrameStats {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut state = serializer.serialize_struct("FrameStats", 21)?;
        state.serialize_field("fps", &self.fps)?;
        state.serialize_field("avg_ms", &self.avg_ms)?;
        state.serialize_field("work_ms", &self.work_ms)?;
        state.serialize_field("canvas_cpu_ms", &self.canvas_cpu_ms)?;
        state.serialize_field("alloc_count", &self.allocation_stats.alloc_count)?;
        state.serialize_field("alloc_bytes", &self.allocation_stats.alloc_bytes)?;
        state.serialize_field("dealloc_count", &self.allocation_stats.dealloc_count)?;
        state.serialize_field("dealloc_bytes", &self.allocation_stats.dealloc_bytes)?;
        state.serialize_field("realloc_count", &self.allocation_stats.realloc_count)?;
        state.serialize_field("min_ms", &self.min_ms)?;
        state.serialize_field("max_ms", &self.max_ms)?;
        state.serialize_field("p50_ms", &self.p50_ms)?;
        state.serialize_field("p95_ms", &self.p95_ms)?;
        state.serialize_field("p99_ms", &self.p99_ms)?;
        state.serialize_field("jank_percent", &self.jank_percent)?;
        state.serialize_field("frame_jank_count", &self.frame_jank_count)?;
        state.serialize_field("variance_ms", &self.variance_ms)?;
        state.serialize_field("frame_count", &self.frame_count)?;
        state.serialize_field("total_frames", &self.total_frames)?;
        state.serialize_field("total_jank_frames", &self.total_jank_frames)?;
        state.end()
    }
}

/// Initialize the FPS profiler (call once at startup)
pub fn init() {
    let env_overlay = echo_core::env::flag("ECHO_FPS_OVERLAY");
    let env_measurements =
        echo_core::env::flag("ECHO_MEASUREMENTS") || echo_core::env::flag("ZED_MEASUREMENTS");
    let env_profiler = echo_core::env::flag("ECHO_FPS_PROFILER");

    let enabled = env_profiler || env_overlay || env_measurements;
    PROFILER_ENABLED.store(enabled, Ordering::Relaxed);
    OVERLAY_ENABLED.store(env_overlay && enabled, Ordering::Relaxed);
    MEASUREMENTS_ENABLED.store(env_measurements && enabled, Ordering::Relaxed);

    if !enabled {
        return;
    }

    if env_overlay {
        crate::log_perf!(info, "Overlay enabled");
    }
    if env_measurements {
        crate::log_perf!(info, "Measurements output enabled");
    }

    let mut profiler = PROFILER.write_or_recover();
    if let Some(ms) = echo_core::env::parsed::<f64>("ECHO_JANK_THRESHOLD") {
        profiler.jank_threshold_ms = ms;
        crate::log_perf!(info, "Jank threshold: {}ms", ms);
    }
    profiler.init();
}

/// Begin a new frame - call at start of render()
/// Returns a frame ID to pass to end_frame()
pub fn begin_frame() -> u64 {
    if !is_enabled() {
        return 0;
    }
    PROFILER.write_or_recover().begin_frame()
}

/// Record a phase timing within the current frame
pub fn record_phase(name: &'static str, duration: Duration) {
    if !is_enabled() {
        return;
    }
    PROFILER.write_or_recover().record_phase(name, duration);
}

/// Start a phase timer only while frame diagnostics are active.
#[inline]
pub fn phase_timer() -> Option<Instant> {
    is_enabled().then(Instant::now)
}

/// Record a phase from an optional timer returned by [`phase_timer`].
#[inline]
pub fn record_phase_since(name: &'static str, started_at: Option<Instant>) {
    if let Some(started_at) = started_at {
        record_phase(name, started_at.elapsed());
    }
}

/// End the current frame - call at end of render()
pub fn end_frame() -> Option<FrameTiming> {
    if !is_enabled() {
        return None;
    }
    PROFILER.write_or_recover().end_frame()
}

/// Get current frame statistics
pub fn get_stats() -> FrameStats {
    if !is_enabled() {
        return FrameStats::default();
    }
    PROFILER.read_or_recover().get_stats()
}

/// Check if frame profiler is enabled.
pub fn is_enabled() -> bool {
    PROFILER_ENABLED.load(Ordering::Relaxed) || DASHBOARD_ENABLED.load(Ordering::Relaxed)
}

/// Enable frame diagnostics while the full metrics dashboard is visible.
pub fn set_dashboard_enabled(enabled: bool) {
    DASHBOARD_ENABLED.store(enabled, Ordering::Relaxed);
    if enabled && let Ok(mut profiler) = PROFILER.write() {
        profiler.init();
    }
}

/// Check if overlay display is enabled
pub fn is_overlay_enabled() -> bool {
    is_enabled() && OVERLAY_ENABLED.load(Ordering::Relaxed)
}

/// Enable or disable overlay display at runtime
pub fn set_overlay_enabled(enabled: bool) {
    if enabled && !is_enabled() {
        PROFILER_ENABLED.store(true, Ordering::Relaxed);
        if let Ok(mut profiler) = PROFILER.write() {
            profiler.init();
        }
    }
    OVERLAY_ENABLED.store(enabled, Ordering::Relaxed);
}

/// Record the real CPU paint-prep cost of one `CanvasMessageList::render()` pass.
///
/// Recorded unconditionally (the store is a single relaxed atomic write, cheap enough to
/// stay off the hot-path budget) so `canvas_cpu_ms` reflects honest cost even when the
/// frame profiler itself is disabled. Independent of poll cadence.
#[inline]
pub fn record_canvas_cpu_nanos(nanos: u64) {
    CANVAS_CPU_NANOS.store(nanos, Ordering::Relaxed);
    LAST_CANVAS_PAINT_REL_MS.store(
        (CANVAS_PAINT_EPOCH.elapsed().as_millis() as u64).max(1),
        Ordering::Relaxed,
    );
}

/// Record that the window presented a frame without a canvas render pass.
///
/// Animation frames patched into the presented scene
/// (`Window::patch_animated_image`) reach the screen without rendering any
/// canvas, so the animation waker reports observed presents here to keep
/// [`canvas_paint_stalled`] from mistaking a presenting window for a hidden one.
#[inline]
pub fn record_frame_presented() {
    LAST_CANVAS_PAINT_REL_MS.store(
        (CANVAS_PAINT_EPOCH.elapsed().as_millis() as u64).max(1),
        Ordering::Relaxed,
    );
}

/// Millisecond timestamp (relative to [`CANVAS_PAINT_EPOCH`]) of the most recent
/// canvas render pass or observed present. `0` means none has been recorded yet.
static LAST_CANVAS_PAINT_REL_MS: AtomicU64 = AtomicU64::new(0);

static CANVAS_PAINT_EPOCH: std::sync::LazyLock<Instant> = std::sync::LazyLock::new(Instant::now);

/// How long the canvas can go without painting before the window is considered
/// not-presenting (minimized or hidden).
const CANVAS_PAINT_STALL_MS: u64 = 1000;

/// True when no canvas paint has landed for over [`CANVAS_PAINT_STALL_MS`]
/// despite at least one paint having happened before.
///
/// GPUI's Windows backend parks its `request_frame` callback while the window is
/// minimized, so `cx.notify()` cannot become a frame. Wake loops and the frame
/// rate controller use this to pause animation pacing instead of notifying into
/// the void.
pub fn canvas_paint_stalled() -> bool {
    let last = LAST_CANVAS_PAINT_REL_MS.load(Ordering::Relaxed);
    if last == 0 {
        return false;
    }
    let now = CANVAS_PAINT_EPOCH.elapsed().as_millis() as u64;
    now.saturating_sub(last) > CANVAS_PAINT_STALL_MS
}

/// Most recent canvas paint-prep CPU cost in milliseconds (see [`record_canvas_cpu_nanos`]).
#[inline]
pub fn canvas_cpu_ms() -> f64 {
    CANVAS_CPU_NANOS.load(Ordering::Relaxed) as f64 / 1_000_000.0
}

/// Record the real GPU paint-emit cost of one `ChatViewportElement::paint()` pass.
///
/// Recorded unconditionally (the store is a single relaxed atomic write, like
/// [`record_canvas_cpu_nanos`]) so `canvas_paint_ms` reflects honest cost even when the
/// frame profiler itself is disabled. This is the actual paint pass GPUI runs *after*
/// `render()` returns. Independent of poll cadence.
#[inline]
pub fn record_canvas_paint_nanos(nanos: u64) {
    CANVAS_PAINT_NANOS.store(nanos, Ordering::Relaxed);
}

/// Frame-path classes recorded by the renderer for the 30-frame breakdown log.
///
/// Mirrors the mutually-exclusive fast/slow paths a canvas render frame can take. Bumped
/// once per frame via [`record_frame_classification`].
#[derive(Clone, Copy, Debug)]
pub enum FrameClass {
    /// Visible rows reused unchanged (no layout, no re-prep).
    StableReuse,
    /// Visible rows translated for a scroll-only frame.
    ScrollTranslate,
    /// Full layout/visible-row rebuild (worst case).
    FullRebuild,
    /// Stable-reuse frame driven solely by an animation tick.
    AnimationOnly,
}

/// Record this frame's classification so the 30-frame breakdown log can report the path
/// mix. Cheap (one relaxed atomic increment); still gated to avoid touching the atomics in
/// normal (profiler-disabled) runs.
#[inline]
pub fn record_frame_classification(class: FrameClass) {
    if !is_enabled() {
        return;
    }
    let counter = match class {
        FrameClass::StableReuse => &FRAME_CLASS_STABLE_REUSE,
        FrameClass::ScrollTranslate => &FRAME_CLASS_SCROLL_TRANSLATE,
        FrameClass::FullRebuild => &FRAME_CLASS_FULL_REBUILD,
        FrameClass::AnimationOnly => &FRAME_CLASS_ANIMATION_ONLY,
    };
    counter.fetch_add(1, Ordering::Relaxed);
}

/// Cumulative cadence-independent dropped-frame count (render-start gaps > 20ms).
#[inline]
pub fn frame_jank_count() -> u64 {
    FRAME_JANK_COUNT.load(Ordering::Relaxed)
}

/// Keep a secondary window's diagnostic GPU query ring off.
///
/// Called from canvas render, which also runs inside popout windows. Only the
/// main window feeds the process-global dashboard, so this is a no-op there.
#[inline]
pub fn disable_secondary_window_gpu_timing(window: &gpui::Window) {
    if !crate::theme::colors::main_window_frame_active() {
        window.set_gpu_timing_enabled(false);
    }
}

/// Per-frame averages of GPUI's cumulative frame-stage counters between two snapshots.
#[derive(Clone, Copy, Debug, Default)]
struct FrameStageDelta {
    frames: u64,
    presents: u64,
    partial_presents: u64,
    animation_patches: u64,
    patch_rejections: [u64; 5],
    draw_ms: f64,
    layout_paint_ms: f64,
    request_layout_ms: f64,
    prepaint_ms: f64,
    paint_ms: f64,
    views_rendered: f64,
    views_reused: f64,
    scene_finish_ms: f64,
    present_ms: f64,
    primitives: f64,
    replayed_pct: f64,
    renderer: Option<RendererStageDelta>,
}

/// Per-frame averages of the platform renderer's cumulative CPU counters.
#[derive(Clone, Copy, Debug, Default)]
struct RendererStageDelta {
    upload_ms: f64,
    encode_ms: f64,
    present_call_ms: f64,
    upload_kb: f64,
}

impl FrameStageDelta {
    /// `None` when no frame was drawn between the snapshots.
    fn between(
        previous: &gpui::FrameStageStatistics,
        current: &gpui::FrameStageStatistics,
    ) -> Option<Self> {
        let presents = current
            .frames_presented
            .checked_sub(previous.frames_presented)?;
        if presents == 0 {
            return None;
        }
        let frames = current
            .frames_drawn
            .saturating_sub(previous.frames_drawn)
            .max(1);
        let per_frame_ms =
            |now: u64, then: u64| now.saturating_sub(then) as f64 / 1e6 / frames as f64;
        let presented = current
            .frames_presented
            .saturating_sub(previous.frames_presented)
            .max(1);
        let primitives = current.primitives.saturating_sub(previous.primitives);
        let replayed = current
            .replayed_primitives
            .saturating_sub(previous.replayed_primitives);
        let renderer = match (previous.renderer, current.renderer) {
            (Some(previous), Some(current)) => {
                let renderer_frames = current.frames.saturating_sub(previous.frames).max(1) as f64;
                let per_renderer_frame_ms =
                    |now: u64, then: u64| now.saturating_sub(then) as f64 / 1e6 / renderer_frames;
                Some(RendererStageDelta {
                    upload_ms: per_renderer_frame_ms(current.upload_nanos, previous.upload_nanos),
                    encode_ms: per_renderer_frame_ms(current.encode_nanos, previous.encode_nanos),
                    present_call_ms: per_renderer_frame_ms(
                        current.present_call_nanos,
                        previous.present_call_nanos,
                    ),
                    upload_kb: current.upload_bytes.saturating_sub(previous.upload_bytes) as f64
                        / 1024.0
                        / renderer_frames,
                })
            }
            _ => None,
        };
        Some(Self {
            frames,
            presents,
            partial_presents: current
                .partial_presents
                .saturating_sub(previous.partial_presents),
            animation_patches: current
                .animated_image_patches
                .saturating_sub(previous.animated_image_patches),
            patch_rejections: std::array::from_fn(|reason| {
                current.animated_image_patch_rejections[reason]
                    .saturating_sub(previous.animated_image_patch_rejections[reason])
            }),
            draw_ms: per_frame_ms(current.draw_nanos, previous.draw_nanos),
            layout_paint_ms: per_frame_ms(current.layout_paint_nanos, previous.layout_paint_nanos),
            request_layout_ms: per_frame_ms(
                current.request_layout_nanos,
                previous.request_layout_nanos,
            ),
            prepaint_ms: per_frame_ms(current.prepaint_nanos, previous.prepaint_nanos),
            paint_ms: per_frame_ms(current.paint_nanos, previous.paint_nanos),
            views_rendered: current
                .views_rendered
                .saturating_sub(previous.views_rendered) as f64
                / frames as f64,
            views_reused: current.views_reused.saturating_sub(previous.views_reused) as f64
                / frames as f64,
            scene_finish_ms: per_frame_ms(current.scene_finish_nanos, previous.scene_finish_nanos),
            present_ms: current.present_nanos.saturating_sub(previous.present_nanos) as f64
                / 1e6
                / presented as f64,
            primitives: primitives as f64 / frames as f64,
            replayed_pct: if primitives == 0 {
                0.0
            } else {
                replayed as f64 * 100.0 / primitives as f64
            },
            renderer,
        })
    }
}

/// Record the latest main-window GPU and swap-chain statistics from GPUI.
///
/// Called once per main-window draw by the main window's root view, so samples
/// keep advancing when no chat canvas re-renders. The caller guarantees `window`
/// is the main window. This toggles the fork's diagnostic-only GPU query ring,
/// reads the latest asynchronously resolved GPU duration, and samples swap-chain
/// presentation statistics. Missing readings clear their respective sentinels so
/// the overlay never presents stale values as current measurements.
#[inline]
pub fn record_main_window_renderer_statistics(window: &gpui::Window) {
    let enabled = is_enabled();
    window.set_gpu_timing_enabled(enabled);
    if !enabled {
        GPU_FRAME_MS_BITS.store(GPU_FRAME_MS_NONE, Ordering::Relaxed);
        GPU_FRAME_SAMPLE_COUNT.store(0, Ordering::Relaxed);
        PRESENT_LATENCY_BITS.store(PRESENT_LATENCY_NONE, Ordering::Relaxed);
        GPUI_DRAW_DURATION_SAMPLE_COUNT.store(0, Ordering::Relaxed);
        GPUI_PRESENT_INTERVAL_SAMPLE_COUNT.store(0, Ordering::Relaxed);
        return;
    }

    record_gpui_frame_duration_statistics(window);
    MAIN_WINDOW_ID.store(
        window.window_handle().window_id().as_u64(),
        Ordering::Relaxed,
    );
    sample_frame_stages(window);

    match window.gpu_frame_statistics() {
        Some(stats) => {
            GPU_FRAME_MS_BITS.store(stats.gpu_frame_time_ms.to_bits(), Ordering::Relaxed);
            GPU_FRAME_SAMPLE_COUNT.store(stats.sample_count, Ordering::Relaxed);
        }
        None => {
            GPU_FRAME_MS_BITS.store(GPU_FRAME_MS_NONE, Ordering::Relaxed);
            GPU_FRAME_SAMPLE_COUNT.store(0, Ordering::Relaxed);
        }
    }

    match window.present_statistics() {
        Some(stats) => {
            PRESENT_LATENCY_BITS.store(stats.present_latency_ms.to_bits(), Ordering::Relaxed);
            PRESENT_COUNT.store(stats.present_count as u64, Ordering::Relaxed);
        }
        None => {
            PRESENT_LATENCY_BITS.store(PRESENT_LATENCY_NONE, Ordering::Relaxed);
        }
    }
}

/// Copy a bounded summary from GPUI's upstream frame-duration telemetry.
///
/// GPUI itself records draw duration for every `Window::draw` and present intervals only for
/// consecutive active animation frames. Echo never uses either value for pacing, throttling, or
/// product behavior: this is diagnostics-only visibility into the framework boundary.
#[inline]
#[cfg(feature = "gpui-frame-duration-histogram")]
fn record_gpui_frame_duration_statistics(window: &gpui::Window) {
    let now_ms = GPUI_FRAME_DURATION_SNAPSHOT_EPOCH.elapsed().as_millis() as u64;
    let last_ms = GPUI_FRAME_DURATION_LAST_SNAPSHOT_MS.load(Ordering::Relaxed);
    if now_ms.saturating_sub(last_ms) < GPUI_FRAME_DURATION_SNAPSHOT_INTERVAL_MS {
        return;
    }
    GPUI_FRAME_DURATION_LAST_SNAPSHOT_MS.store(now_ms, Ordering::Relaxed);

    let snapshot = window.frame_duration_snapshot();
    let draw = snapshot.draw_duration_histogram;
    GPUI_DRAW_DURATION_SAMPLE_COUNT.store(draw.len(), Ordering::Relaxed);
    GPUI_DRAW_DURATION_P50_MS_BITS.store(
        (draw.value_at_quantile(0.50) as f64 / 1_000_000.0).to_bits(),
        Ordering::Relaxed,
    );
    GPUI_DRAW_DURATION_P95_MS_BITS.store(
        (draw.value_at_quantile(0.95) as f64 / 1_000_000.0).to_bits(),
        Ordering::Relaxed,
    );

    let present = snapshot.present_interval_histogram;
    GPUI_PRESENT_INTERVAL_SAMPLE_COUNT.store(present.len(), Ordering::Relaxed);
    GPUI_PRESENT_INTERVAL_P50_MS_BITS.store(
        (present.value_at_quantile(0.50) as f64 / 1_000_000.0).to_bits(),
        Ordering::Relaxed,
    );
    GPUI_PRESENT_INTERVAL_P95_MS_BITS.store(
        (present.value_at_quantile(0.95) as f64 / 1_000_000.0).to_bits(),
        Ordering::Relaxed,
    );
}

#[inline]
#[cfg(not(feature = "gpui-frame-duration-histogram"))]
fn record_gpui_frame_duration_statistics(_window: &gpui::Window) {}

/// A diagnostics-only summary of the upstream GPUI frame-duration histograms.
#[derive(Clone, Copy, Debug)]
pub struct GpuiFrameDurationStats {
    /// Number of recorded `Window::draw` durations.
    pub draw_samples: u64,
    /// Median and p95 `Window::draw` duration in milliseconds.
    pub draw_p50_ms: f64,
    pub draw_p95_ms: f64,
    /// Number of intervals between consecutive active animation-frame presents.
    pub animation_present_samples: u64,
    /// Median and p95 active-animation present interval in milliseconds, if animations ran.
    pub animation_present_p50_ms: Option<f64>,
    pub animation_present_p95_ms: Option<f64>,
}

/// Return the latest sampled GPUI frame-duration summary while diagnostics are active.
///
/// `None` means this binary lacks the opt-in histogram feature, diagnostics are disabled, or GPUI
/// has not drawn a frame since the panel was enabled. In instrumented builds the histogram itself
/// is cumulative over the lifetime of the window.
#[inline]
pub fn gpui_frame_duration_stats() -> Option<GpuiFrameDurationStats> {
    let draw_samples = GPUI_DRAW_DURATION_SAMPLE_COUNT.load(Ordering::Relaxed);
    if !is_enabled() || draw_samples == 0 {
        return None;
    }
    let animation_present_samples = GPUI_PRESENT_INTERVAL_SAMPLE_COUNT.load(Ordering::Relaxed);
    Some(GpuiFrameDurationStats {
        draw_samples,
        draw_p50_ms: f64::from_bits(GPUI_DRAW_DURATION_P50_MS_BITS.load(Ordering::Relaxed)),
        draw_p95_ms: f64::from_bits(GPUI_DRAW_DURATION_P95_MS_BITS.load(Ordering::Relaxed)),
        animation_present_samples,
        animation_present_p50_ms: (animation_present_samples > 0)
            .then(|| f64::from_bits(GPUI_PRESENT_INTERVAL_P50_MS_BITS.load(Ordering::Relaxed))),
        animation_present_p95_ms: (animation_present_samples > 0)
            .then(|| f64::from_bits(GPUI_PRESENT_INTERVAL_P95_MS_BITS.load(Ordering::Relaxed))),
    })
}

/// GPU execution time in milliseconds for the most recently completed GPUI frame.
///
/// The value is produced by GPUI's Windows Direct3D 11 or supported native Linux wgpu
/// timestamp-query ring and read back asynchronously. `None` before the first sample
/// completes, when diagnostics are disabled, or on an unsupported backend or adapter.
#[inline]
pub fn gpu_ms() -> Option<f64> {
    let bits = GPU_FRAME_MS_BITS.load(Ordering::Relaxed);
    if bits == GPU_FRAME_MS_NONE {
        None
    } else {
        Some(f64::from_bits(bits))
    }
}

/// Running count of successfully resolved, non-disjoint GPU timestamp samples.
#[inline]
pub fn gpu_sample_count() -> Option<u64> {
    gpu_ms().map(|_| GPU_FRAME_SAMPLE_COUNT.load(Ordering::Relaxed))
}

/// Present (swap-chain) latency in milliseconds for the most recently scanned-out frame.
///
/// Sourced from GPUI's `Window::present_statistics` (`echo-paint-fork`) and recorded by
/// [`record_main_window_renderer_statistics`]. `None` when no honest reading is available (non-Windows,
/// non-flip-model swap chain, statistics disjoint, or before the first vsync-aligned
/// present).
#[inline]
pub fn present_latency_ms() -> Option<f64> {
    let bits = PRESENT_LATENCY_BITS.load(Ordering::Relaxed);
    if bits == PRESENT_LATENCY_NONE {
        None
    } else {
        Some(f64::from_bits(bits))
    }
}

/// Running swap-chain present count from the most recent reading, if any has been taken.
///
/// `None` until the first successful [`record_main_window_renderer_statistics`] reading. Useful for
/// confirming the present-statistics path is live (the count advances every frame).
#[inline]
pub fn present_count() -> Option<u64> {
    if present_latency_ms().is_some() {
        Some(PRESENT_COUNT.load(Ordering::Relaxed))
    } else {
        None
    }
}

use gpui::*;

/// Render the FPS overlay as a GPUI element
/// Call this from your root view's render() method if overlay is enabled
pub fn render_fps_overlay() -> impl IntoElement {
    let stats = get_stats();
    let fps_color = if stats.fps >= 55.0 {
        rgb(0x4ADE80)
    } else if stats.fps >= 30.0 {
        rgb(0xFBBF24)
    } else {
        rgb(0xF87171)
    };

    div()
        .id("fps-overlay")
        .absolute()
        .top_2()
        .left_2()
        .px_2()
        .py_1()
        .rounded(px(4.0))
        .bg(rgba(0x000000AA))
        .border_1()
        .border_color(rgba(0x333333FF))
        .child(
            div()
                .flex()
                .flex_col()
                .gap_0p5()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(
                            div()
                                .text_xs()
                                .font_weight(FontWeight::BOLD)
                                .text_color(fps_color)
                                .child(format!("{:.0} FPS", stats.fps)),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(rgba(0xAAAAAAFF))
                                .child(format!("{:.1}ms", stats.avg_ms)),
                        ),
                )
                .child(div().text_xs().text_color(rgba(0x888888FF)).child(format!(
                    "p95: {:.1}ms | jank: {:.0}%",
                    stats.p95_ms, stats.jank_percent
                )))
                .child(div().text_xs().text_color(rgba(0x888888FF)).child(format!(
                    "allocs: {} / {} bytes",
                    stats.allocation_stats.alloc_count, stats.allocation_stats.alloc_bytes
                ))),
        )
}

/// RAII guard that automatically begins/ends frame timing
pub struct FrameTimingGuard {
    frame_started: bool,
}

impl FrameTimingGuard {
    /// Begin timing a frame (call at start of render)
    pub fn begin() -> Self {
        if is_enabled() {
            begin_frame();
            return Self {
                frame_started: true,
            };
        }
        Self {
            frame_started: false,
        }
    }
}

impl Drop for FrameTimingGuard {
    fn drop(&mut self) {
        if self.frame_started {
            end_frame();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test;
    use std::thread::sleep;

    #[test]
    fn test_frame_timing() {
        init();
        set_dashboard_enabled(true);

        for _ in 0..10 {
            begin_frame();
            sleep(Duration::from_millis(5));
            record_phase("test", Duration::from_millis(3));
            end_frame();
        }

        let stats = get_stats();
        set_dashboard_enabled(false);
        assert!(stats.fps > 0.0);
        assert!(stats.avg_ms > 0.0);
        assert!(stats.frame_count > 0);
    }

    #[test]
    fn test_percentiles() {
        let sorted = vec![1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0, 10.0];
        assert!((percentile(&sorted, 0.50) - 6.0).abs() < 0.5);
        assert!((percentile(&sorted, 0.95) - 10.0).abs() < 0.5);
    }
}
