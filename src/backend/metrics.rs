//! Frontend metrics module for performance monitoring.
//!
//! Tracks message processing times, render counts, and emote loading statistics.

use std::cmp::Ordering as CmpOrdering;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{LazyLock, RwLock};
use std::time::Duration;

/// Frontend performance metrics
pub struct FrontendMetrics {
    /// Total messages received from IRC
    pub messages_received: u64,
    /// Total messages rendered to DOM
    pub messages_rendered: u64,
    /// Rolling window of batch processing times (ms)
    pub batch_times_ms: Vec<f64>,
    /// Successful emote loads
    pub emote_loads: u64,
    /// Failed emote loads
    pub emote_failures: u64,
    /// Last recorded batch size
    pub last_batch_size: usize,
    /// Messages per second (rolling average)
    pub messages_per_second: f64,
}

impl Default for FrontendMetrics {
    fn default() -> Self {
        Self {
            messages_received: 0,
            messages_rendered: 0,
            batch_times_ms: Vec::with_capacity(100),
            emote_loads: 0,
            emote_failures: 0,
            last_batch_size: 0,
            messages_per_second: 0.0,
        }
    }
}

/// Global metrics storage
pub static METRICS: LazyLock<RwLock<FrontendMetrics>> =
    LazyLock::new(|| RwLock::new(FrontendMetrics::default()));

/// Whether runtime performance metrics collection is enabled.
/// Defaults to disabled to avoid overhead in production sessions.
static METRICS_ENABLED: AtomicBool = AtomicBool::new(false);

/// Total UI notify calls routed through instrumented hot paths.
static UI_NOTIFY_COUNT: AtomicU64 = AtomicU64::new(0);

/// Total app render passes (root render calls).
static APP_RENDER_COUNT: AtomicU64 = AtomicU64::new(0);
/// App renders that followed an instrumented notify edge.
static APP_RENDER_FROM_NOTIFY_COUNT: AtomicU64 = AtomicU64::new(0);
/// App renders with no new instrumented notify since previous render.
static APP_RENDER_EXTERNAL_COUNT: AtomicU64 = AtomicU64::new(0);
/// Monotonic sequence incremented for each instrumented notify.
static UI_NOTIFY_SEQ: AtomicU64 = AtomicU64::new(0);
/// Last notify sequence observed by app render accounting.
static LAST_RENDER_SEEN_NOTIFY_SEQ: AtomicU64 = AtomicU64::new(0);

/// Per-split counters for render/update attribution.
static SPLIT_COUNTERS: LazyLock<RwLock<HashMap<String, SplitCounters>>> =
    LazyLock::new(|| RwLock::new(HashMap::with_capacity(32)));

/// UI notify counters by source label.
static NOTIFY_SOURCES: LazyLock<RwLock<HashMap<String, u64>>> =
    LazyLock::new(|| RwLock::new(HashMap::with_capacity(64)));

/// Emote pipeline stage timings keyed by stage label.
static EMOTE_STAGE_METRICS: LazyLock<RwLock<HashMap<String, EmoteStageCounters>>> =
    LazyLock::new(|| RwLock::new(HashMap::with_capacity(64)));

/// Emote pipeline event counters keyed by event label.
static EMOTE_EVENT_COUNTERS: LazyLock<RwLock<HashMap<&'static str, u64>>> =
    LazyLock::new(|| RwLock::new(HashMap::with_capacity(64)));

/// Aggregated canvas layout/cache counters from renderer frames.
static LAYOUT_CACHE_HITS: AtomicU64 = AtomicU64::new(0);
static LAYOUT_CACHE_MISSES: AtomicU64 = AtomicU64::new(0);
/// Per-source layout hit counters (subdivision of LAYOUT_CACHE_HITS + async promotions).
static LAYOUT_HIT_SPLIT_CACHE: AtomicU64 = AtomicU64::new(0);
static LAYOUT_HIT_GLOBAL_CACHE: AtomicU64 = AtomicU64::new(0);
static LAYOUT_HIT_ASYNC_READY: AtomicU64 = AtomicU64::new(0);
static LAYOUT_MISS_SYNC_SHAPE: AtomicU64 = AtomicU64::new(0);
static LAYOUT_ASYNC_REQUESTS: AtomicU64 = AtomicU64::new(0);
static LAYOUT_ASYNC_READY_VALID_WIDTH: AtomicU64 = AtomicU64::new(0);
static LAYOUT_ASYNC_READY_STALE_WIDTH: AtomicU64 = AtomicU64::new(0);
static LAYOUT_FULL_REBUILDS: AtomicU64 = AtomicU64::new(0);
static LAYOUT_MESSAGE_ORDER_RESETS: AtomicU64 = AtomicU64::new(0);
static LAYOUT_REBUILD_REASON_ORDER: AtomicU64 = AtomicU64::new(0);
static LAYOUT_REBUILD_REASON_HEIGHT: AtomicU64 = AtomicU64::new(0);
static LAYOUT_REBUILD_REASON_STALE: AtomicU64 = AtomicU64::new(0);
static LAYOUT_REBUILD_REASON_FORCE: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Debug, Default)]
pub struct SplitCounters {
    pub renders: u64,
    pub updates: u64,
    pub messages_delta: u64,
}

#[derive(Clone, Debug)]
pub struct SplitMetricRow {
    pub split: String,
    pub renders: u64,
    pub updates: u64,
    pub messages_delta: u64,
}

#[derive(Clone, Debug)]
pub struct NotifySourceRow {
    pub source: String,
    pub count: u64,
}

#[derive(Clone, Debug, Default)]
pub struct EmoteStageCounters {
    pub count: u64,
    pub total_ms: f64,
    pub max_ms: f64,
    pub last_ms: f64,
    pub total_items: u64,
    pub last_items: u64,
}

#[derive(Clone, Debug)]
pub struct EmoteStageRow {
    pub stage: String,
    pub count: u64,
    pub total_ms: f64,
    pub avg_ms: f64,
    pub max_ms: f64,
    pub last_ms: f64,
    pub total_items: u64,
    pub avg_items: f64,
    pub last_items: u64,
}

#[derive(Clone, Debug, Default)]
pub struct LayoutFrameMetrics {
    pub cache_hits: u64,
    pub cache_misses: u64,
    /// Hits satisfied from the per-split local LRU cache.
    pub hit_split_cache: u64,
    /// Hits satisfied from the cross-split GlobalLayoutCache.
    pub hit_global_cache: u64,
    /// Hits satisfied by promoting a freshly-completed async result.
    pub hit_async_ready: u64,
    /// Layouts built synchronously on the main thread (worst-case miss).
    pub miss_sync_shape: u64,
    pub async_requests: u64,
    pub async_ready_valid_width: u64,
    pub async_ready_stale_width: u64,
    pub full_rebuilds: u64,
    pub message_order_resets: u64,
    pub rebuild_reason_order: bool,
    pub rebuild_reason_height: bool,
    pub rebuild_reason_stale: bool,
    pub rebuild_reason_force: bool,
}

#[derive(Clone, Debug, Default)]
pub struct LayoutMetricsSummary {
    pub cache_hits: u64,
    pub cache_misses: u64,
    pub hit_split_cache: u64,
    pub hit_global_cache: u64,
    pub hit_async_ready: u64,
    pub miss_sync_shape: u64,
    pub async_requests: u64,
    pub async_ready_valid_width: u64,
    pub async_ready_stale_width: u64,
    pub full_rebuilds: u64,
    pub message_order_resets: u64,
    pub rebuild_reason_order: u64,
    pub rebuild_reason_height: u64,
    pub rebuild_reason_stale: u64,
    pub rebuild_reason_force: u64,
}

/// Initialize runtime metrics collection.
///
/// Enable with `ECHO_PERF_MONITOR=1` when you need diagnostics.
pub fn init() {
    let env_enabled = echo_core::env::flag("ECHO_PERF_MONITOR");
    let profiler_enabled = crate::diagnostics::fps_profiler::is_enabled();
    let enabled = env_enabled || profiler_enabled;
    METRICS_ENABLED.store(enabled, Ordering::Relaxed);
    if enabled {
        crate::log_perf!(
            info,
            env_enabled,
            profiler_enabled,
            "Runtime performance metrics enabled"
        );
    }
}

/// Returns true when runtime metrics collection is enabled.
#[inline]
pub fn is_enabled() -> bool {
    METRICS_ENABLED.load(Ordering::Relaxed) || crate::diagnostics::fps_profiler::is_enabled()
}

/// Get metrics summary
pub fn get_summary() -> MetricsSummary {
    if !is_enabled() {
        return MetricsSummary::default();
    }
    METRICS
        .read()
        .ok()
        .map(|m| MetricsSummary {
            messages_received: m.messages_received,
            messages_rendered: m.messages_rendered,
            messages_per_second: m.messages_per_second,
            avg_batch_time_ms: if m.batch_times_ms.is_empty() {
                0.0
            } else {
                m.batch_times_ms.iter().sum::<f64>() / m.batch_times_ms.len() as f64
            },
            emote_loads: m.emote_loads,
            emote_failures: m.emote_failures,
            emote_success_rate: if m.emote_loads + m.emote_failures == 0 {
                100.0
            } else {
                (m.emote_loads as f64 / (m.emote_loads + m.emote_failures) as f64) * 100.0
            },
            ui_notify_count: UI_NOTIFY_COUNT.load(Ordering::Relaxed),
            app_render_count: APP_RENDER_COUNT.load(Ordering::Relaxed),
            app_render_from_notify_count: APP_RENDER_FROM_NOTIFY_COUNT.load(Ordering::Relaxed),
            app_render_external_count: APP_RENDER_EXTERNAL_COUNT.load(Ordering::Relaxed),
            layout: layout_metrics_summary(),
        })
        .unwrap_or_default()
}

/// Reset all metrics
pub fn reset_metrics() {
    if !is_enabled() {
        return;
    }
    if let Ok(mut m) = METRICS.write() {
        m.messages_received = 0;
        m.messages_rendered = 0;
        m.batch_times_ms.clear();
        m.emote_loads = 0;
        m.emote_failures = 0;
        m.last_batch_size = 0;
        m.messages_per_second = 0.0;
    }
    UI_NOTIFY_COUNT.store(0, Ordering::Relaxed);
    APP_RENDER_COUNT.store(0, Ordering::Relaxed);
    APP_RENDER_FROM_NOTIFY_COUNT.store(0, Ordering::Relaxed);
    APP_RENDER_EXTERNAL_COUNT.store(0, Ordering::Relaxed);
    UI_NOTIFY_SEQ.store(0, Ordering::Relaxed);
    LAST_RENDER_SEEN_NOTIFY_SEQ.store(0, Ordering::Relaxed);
    LAYOUT_CACHE_HITS.store(0, Ordering::Relaxed);
    LAYOUT_CACHE_MISSES.store(0, Ordering::Relaxed);
    LAYOUT_HIT_SPLIT_CACHE.store(0, Ordering::Relaxed);
    LAYOUT_HIT_GLOBAL_CACHE.store(0, Ordering::Relaxed);
    LAYOUT_HIT_ASYNC_READY.store(0, Ordering::Relaxed);
    LAYOUT_MISS_SYNC_SHAPE.store(0, Ordering::Relaxed);
    LAYOUT_ASYNC_REQUESTS.store(0, Ordering::Relaxed);
    LAYOUT_ASYNC_READY_VALID_WIDTH.store(0, Ordering::Relaxed);
    LAYOUT_ASYNC_READY_STALE_WIDTH.store(0, Ordering::Relaxed);
    LAYOUT_FULL_REBUILDS.store(0, Ordering::Relaxed);
    LAYOUT_MESSAGE_ORDER_RESETS.store(0, Ordering::Relaxed);
    LAYOUT_REBUILD_REASON_ORDER.store(0, Ordering::Relaxed);
    LAYOUT_REBUILD_REASON_HEIGHT.store(0, Ordering::Relaxed);
    LAYOUT_REBUILD_REASON_STALE.store(0, Ordering::Relaxed);
    LAYOUT_REBUILD_REASON_FORCE.store(0, Ordering::Relaxed);
    if let Ok(mut counters) = SPLIT_COUNTERS.write() {
        counters.clear();
    }
    if let Ok(mut sources) = NOTIFY_SOURCES.write() {
        sources.clear();
    }
    if let Ok(mut stages) = EMOTE_STAGE_METRICS.write() {
        stages.clear();
    }
    if let Ok(mut events) = EMOTE_EVENT_COUNTERS.write() {
        events.clear();
    }
}

/// Summary of current metrics
#[derive(Clone, Debug, Default)]
pub struct MetricsSummary {
    pub messages_received: u64,
    pub messages_rendered: u64,
    pub messages_per_second: f64,
    pub avg_batch_time_ms: f64,
    pub emote_loads: u64,
    pub emote_failures: u64,
    pub emote_success_rate: f64,
    pub ui_notify_count: u64,
    pub app_render_count: u64,
    pub app_render_from_notify_count: u64,
    pub app_render_external_count: u64,
    pub layout: LayoutMetricsSummary,
}

/// Record one renderer frame's layout/cache counters.
pub fn record_layout_frame(metrics: LayoutFrameMetrics) {
    if !is_enabled() {
        return;
    }

    add_atomic(&LAYOUT_CACHE_HITS, metrics.cache_hits);
    add_atomic(&LAYOUT_CACHE_MISSES, metrics.cache_misses);
    add_atomic(&LAYOUT_HIT_SPLIT_CACHE, metrics.hit_split_cache);
    add_atomic(&LAYOUT_HIT_GLOBAL_CACHE, metrics.hit_global_cache);
    add_atomic(&LAYOUT_HIT_ASYNC_READY, metrics.hit_async_ready);
    add_atomic(&LAYOUT_MISS_SYNC_SHAPE, metrics.miss_sync_shape);
    add_atomic(&LAYOUT_ASYNC_REQUESTS, metrics.async_requests);
    add_atomic(
        &LAYOUT_ASYNC_READY_VALID_WIDTH,
        metrics.async_ready_valid_width,
    );
    add_atomic(
        &LAYOUT_ASYNC_READY_STALE_WIDTH,
        metrics.async_ready_stale_width,
    );
    add_atomic(&LAYOUT_FULL_REBUILDS, metrics.full_rebuilds);
    add_atomic(&LAYOUT_MESSAGE_ORDER_RESETS, metrics.message_order_resets);

    if metrics.rebuild_reason_order {
        LAYOUT_REBUILD_REASON_ORDER.fetch_add(1, Ordering::Relaxed);
    }
    if metrics.rebuild_reason_height {
        LAYOUT_REBUILD_REASON_HEIGHT.fetch_add(1, Ordering::Relaxed);
    }
    if metrics.rebuild_reason_stale {
        LAYOUT_REBUILD_REASON_STALE.fetch_add(1, Ordering::Relaxed);
    }
    if metrics.rebuild_reason_force {
        LAYOUT_REBUILD_REASON_FORCE.fetch_add(1, Ordering::Relaxed);
    }
}

#[inline]
fn add_atomic(counter: &AtomicU64, value: u64) {
    if value > 0 {
        counter.fetch_add(value, Ordering::Relaxed);
    }
}

fn layout_metrics_summary() -> LayoutMetricsSummary {
    LayoutMetricsSummary {
        cache_hits: LAYOUT_CACHE_HITS.load(Ordering::Relaxed),
        cache_misses: LAYOUT_CACHE_MISSES.load(Ordering::Relaxed),
        hit_split_cache: LAYOUT_HIT_SPLIT_CACHE.load(Ordering::Relaxed),
        hit_global_cache: LAYOUT_HIT_GLOBAL_CACHE.load(Ordering::Relaxed),
        hit_async_ready: LAYOUT_HIT_ASYNC_READY.load(Ordering::Relaxed),
        miss_sync_shape: LAYOUT_MISS_SYNC_SHAPE.load(Ordering::Relaxed),
        async_requests: LAYOUT_ASYNC_REQUESTS.load(Ordering::Relaxed),
        async_ready_valid_width: LAYOUT_ASYNC_READY_VALID_WIDTH.load(Ordering::Relaxed),
        async_ready_stale_width: LAYOUT_ASYNC_READY_STALE_WIDTH.load(Ordering::Relaxed),
        full_rebuilds: LAYOUT_FULL_REBUILDS.load(Ordering::Relaxed),
        message_order_resets: LAYOUT_MESSAGE_ORDER_RESETS.load(Ordering::Relaxed),
        rebuild_reason_order: LAYOUT_REBUILD_REASON_ORDER.load(Ordering::Relaxed),
        rebuild_reason_height: LAYOUT_REBUILD_REASON_HEIGHT.load(Ordering::Relaxed),
        rebuild_reason_stale: LAYOUT_REBUILD_REASON_STALE.load(Ordering::Relaxed),
        rebuild_reason_force: LAYOUT_REBUILD_REASON_FORCE.load(Ordering::Relaxed),
    }
}

/// Record an instrumented UI notify with a source label.
pub fn record_ui_notify_source(source: &str) {
    if !is_enabled() {
        return;
    }
    UI_NOTIFY_COUNT.fetch_add(1, Ordering::Relaxed);
    UI_NOTIFY_SEQ.fetch_add(1, Ordering::Relaxed);
    if let Ok(mut sources) = NOTIFY_SOURCES.write() {
        let entry = sources.entry(source.to_string()).or_insert(0);
        *entry = entry.saturating_add(1);
    }
}

/// Record an app root render.
#[inline]
pub fn record_app_render() {
    if !is_enabled() {
        return;
    }
    APP_RENDER_COUNT.fetch_add(1, Ordering::Relaxed);
    let current_notify_seq = UI_NOTIFY_SEQ.load(Ordering::Relaxed);
    let last_seen = LAST_RENDER_SEEN_NOTIFY_SEQ.load(Ordering::Relaxed);
    if current_notify_seq > last_seen {
        APP_RENDER_FROM_NOTIFY_COUNT.fetch_add(1, Ordering::Relaxed);
        LAST_RENDER_SEEN_NOTIFY_SEQ.store(current_notify_seq, Ordering::Relaxed);
    } else {
        APP_RENDER_EXTERNAL_COUNT.fetch_add(1, Ordering::Relaxed);
    }
}

/// Record split-level render counts (batched).
pub fn record_split_renders(split: &str, count: u64) {
    if !is_enabled() {
        return;
    }
    if count == 0 {
        return;
    }
    if let Ok(mut counters) = SPLIT_COUNTERS.write() {
        let entry = counters.entry(split.to_string()).or_default();
        entry.renders = entry.renders.saturating_add(count);
    }
}

/// Record split-level update counts and message deltas.
pub fn record_split_update(split: &str, messages_delta: u64) {
    if !is_enabled() {
        return;
    }
    if let Ok(mut counters) = SPLIT_COUNTERS.write() {
        let entry = counters.entry(split.to_string()).or_default();
        entry.updates = entry.updates.saturating_add(1);
        entry.messages_delta = entry.messages_delta.saturating_add(messages_delta);
    }
}

/// Keep the `limit` highest-ranked rows in sorted order. The unstable partial
/// select keeps the cost proportional to the map size rather than to a full
/// sort of every recorded label.
fn retain_top_sorted<T>(
    rows: &mut Vec<T>,
    limit: usize,
    mut compare: impl FnMut(&T, &T) -> CmpOrdering,
) {
    if limit == 0 {
        rows.clear();
        return;
    }
    if rows.len() > limit {
        rows.select_nth_unstable_by(limit - 1, &mut compare);
        rows.truncate(limit);
    }
    rows.sort_by(compare);
}

/// Get top split counters sorted by render count.
pub fn top_split_metrics(limit: usize) -> Vec<SplitMetricRow> {
    if !is_enabled() {
        return Vec::new();
    }
    let mut rows = match SPLIT_COUNTERS.read() {
        Ok(counters) => counters
            .iter()
            .map(|(split, c)| SplitMetricRow {
                split: split.clone(),
                renders: c.renders,
                updates: c.updates,
                messages_delta: c.messages_delta,
            })
            .collect::<Vec<_>>(),
        _ => Vec::new(),
    };

    retain_top_sorted(&mut rows, limit, |a, b| {
        b.renders
            .cmp(&a.renders)
            .then_with(|| b.updates.cmp(&a.updates))
            .then_with(|| a.split.cmp(&b.split))
    });
    rows
}

/// Get top UI notify sources sorted by count.
pub fn top_notify_sources(limit: usize) -> Vec<NotifySourceRow> {
    if !is_enabled() {
        return Vec::new();
    }
    let mut rows = match NOTIFY_SOURCES.read() {
        Ok(sources) => sources
            .iter()
            .map(|(source, count)| NotifySourceRow {
                source: source.clone(),
                count: *count,
            })
            .collect::<Vec<_>>(),
        _ => Vec::new(),
    };

    retain_top_sorted(&mut rows, limit, |a, b| {
        b.count.cmp(&a.count).then_with(|| a.source.cmp(&b.source))
    });
    rows
}

/// Most frequent `record_emote_event` counters, highest first.
pub fn top_emote_events(limit: usize) -> Vec<NotifySourceRow> {
    if !is_enabled() {
        return Vec::new();
    }
    let mut rows = match EMOTE_EVENT_COUNTERS.read() {
        Ok(events) => events
            .iter()
            .map(|(event, count)| NotifySourceRow {
                source: (*event).to_string(),
                count: *count,
            })
            .collect::<Vec<_>>(),
        _ => Vec::new(),
    };

    retain_top_sorted(&mut rows, limit, |a, b| {
        b.count.cmp(&a.count).then_with(|| a.source.cmp(&b.source))
    });
    rows
}

/// Record a timing sample for an emote pipeline stage.
pub fn record_emote_stage(stage: &str, elapsed: Duration, items: usize) {
    if !is_enabled() {
        return;
    }

    let elapsed_ms = elapsed.as_secs_f64() * 1000.0;
    if let Ok(mut stages) = EMOTE_STAGE_METRICS.write() {
        let entry = stages.entry(stage.to_string()).or_default();
        entry.count = entry.count.saturating_add(1);
        entry.total_ms += elapsed_ms;
        entry.max_ms = entry.max_ms.max(elapsed_ms);
        entry.last_ms = elapsed_ms;
        entry.total_items = entry.total_items.saturating_add(items as u64);
        entry.last_items = items as u64;
    }
}

/// Get top emote stage metrics sorted by cumulative cost.
pub fn top_emote_stage_metrics(limit: usize) -> Vec<EmoteStageRow> {
    if !is_enabled() {
        return Vec::new();
    }
    let mut rows = match EMOTE_STAGE_METRICS.read() {
        Ok(stages) => stages
            .iter()
            .map(|(stage, s)| EmoteStageRow {
                stage: stage.clone(),
                count: s.count,
                total_ms: s.total_ms,
                avg_ms: if s.count == 0 {
                    0.0
                } else {
                    s.total_ms / s.count as f64
                },
                max_ms: s.max_ms,
                last_ms: s.last_ms,
                total_items: s.total_items,
                avg_items: if s.count == 0 {
                    0.0
                } else {
                    s.total_items as f64 / s.count as f64
                },
                last_items: s.last_items,
            })
            .collect::<Vec<_>>(),
        _ => Vec::new(),
    };

    retain_top_sorted(&mut rows, limit, |a, b| {
        b.total_ms
            .total_cmp(&a.total_ms)
            .then_with(|| b.max_ms.total_cmp(&a.max_ms))
            .then_with(|| a.stage.cmp(&b.stage))
    });
    rows
}

/// Increment a named emote event counter.
pub fn record_emote_event(event: &'static str) {
    if !is_enabled() {
        return;
    }
    if let Ok(mut events) = EMOTE_EVENT_COUNTERS.write() {
        let entry = events.entry(event).or_insert(0);
        *entry = entry.saturating_add(1);
    }
}

/// Memory usage statistics
#[derive(Clone, Debug, Default)]
pub struct MemoryStats {
    /// Estimated bytes used by message buffers
    pub message_buffer_bytes: u64,
    /// Total messages stored across all channels
    pub stored_messages: u64,
    /// Number of active channels
    pub active_channels: u64,
    /// Estimated bytes per message (average)
    pub bytes_per_message: u64,
}

/// Get memory usage statistics for message storage
/// Iterates the map one shard at a time.
pub fn get_memory_stats() -> MemoryStats {
    use super::irc::CHAT_MESSAGES;

    let stored_messages: u64 = CHAT_MESSAGES
        .iter()
        .map(|entry| entry.value().len())
        .sum::<usize>() as u64;
    let active_channels: u64 = CHAT_MESSAGES.len() as u64;

    const ESTIMATED_BYTES_PER_MSG: u64 = 2048;
    let message_buffer_bytes = stored_messages * ESTIMATED_BYTES_PER_MSG;

    MemoryStats {
        message_buffer_bytes,
        stored_messages,
        active_channels,
        bytes_per_message: ESTIMATED_BYTES_PER_MSG,
    }
}
