//! Render trace flags and render-depth guard.

use super::*;
use std::cell::Cell;

const RENDER_TRACE_SLOW_MS: f64 = 8.0;

pub(super) static RENDER_TRACE_ENABLED: LazyLock<bool> = LazyLock::new(|| {
    std::env::var_os("ECHO_RENDER_TRACE").is_some()
        || std::env::var_os("ECHO_RENDER_TRACE_VERBOSE").is_some()
});
static RENDER_TRACE_VERBOSE_ENABLED: LazyLock<bool> =
    LazyLock::new(|| std::env::var_os("ECHO_RENDER_TRACE_VERBOSE").is_some());
pub(super) static SCROLL_SMOKE_TRACE_ENABLED: LazyLock<bool> =
    LazyLock::new(|| std::env::var_os("ECHO_BENCH_SCROLL_SMOKE").is_some());

thread_local! {
    static CANVAS_RENDER_DEPTH: Cell<usize> = const { Cell::new(0) };
}

pub(super) struct CanvasRenderTraceGuard {
    enabled: bool,
    verbose: bool,
    depth: usize,
    started_at: Option<std::time::Instant>,
    channel_id: String,
    finished: bool,
}

impl CanvasRenderTraceGuard {
    pub(super) fn enter(
        channel_id: &str,
        message_count: usize,
        container_width: f32,
        viewport_height: f32,
    ) -> Self {
        let enabled = *RENDER_TRACE_ENABLED;
        if !enabled {
            return Self {
                enabled,
                verbose: false,
                depth: 0,
                started_at: None,
                channel_id: String::new(),
                finished: true,
            };
        }

        let depth = CANVAS_RENDER_DEPTH.with(|depth| {
            let next = depth.get() + 1;
            depth.set(next);
            next
        });
        let verbose = *RENDER_TRACE_VERBOSE_ENABLED;
        if verbose {
            crate::log_perf_render!(
                debug,
                "canvas.render.start depth={} channel={} messages={} width={:.1} height={:.1}",
                depth,
                channel_id,
                message_count,
                container_width,
                viewport_height
            );
        }
        if depth > 1 {
            crate::log_perf_render!(
                warn,
                "canvas.render.reentrant depth={} channel={}",
                depth,
                channel_id
            );
        }

        Self {
            enabled,
            verbose,
            depth,
            started_at: Some(std::time::Instant::now()),
            channel_id: channel_id.to_string(),
            finished: false,
        }
    }

    pub(super) fn finish(&mut self) {
        if !self.enabled || self.finished {
            return;
        }

        self.finished = true;
        let elapsed_ms = self
            .started_at
            .as_ref()
            .map(std::time::Instant::elapsed)
            .unwrap_or_default()
            .as_secs_f64()
            * 1000.0;
        if self.verbose {
            crate::log_perf_render!(
                debug,
                "canvas.render.finish depth={} channel={} elapsed_ms={:.2}",
                self.depth,
                self.channel_id,
                elapsed_ms
            );
        } else if elapsed_ms >= RENDER_TRACE_SLOW_MS {
            crate::log_perf_render!(
                warn,
                "canvas.render.slow depth={} channel={} elapsed_ms={:.2}",
                self.depth,
                self.channel_id,
                elapsed_ms
            );
        }
    }
}

impl Drop for CanvasRenderTraceGuard {
    fn drop(&mut self) {
        if !self.enabled {
            return;
        }

        if !self.finished {
            crate::log_perf_render!(
                error,
                "canvas.render.unfinished depth={} channel={} elapsed_ms={:.2}",
                self.depth,
                self.channel_id,
                self.started_at
                    .as_ref()
                    .map(std::time::Instant::elapsed)
                    .unwrap_or_default()
                    .as_secs_f64()
                    * 1000.0
            );
        }

        CANVAS_RENDER_DEPTH.with(|depth| {
            depth.set(depth.get().saturating_sub(1));
        });
    }
}
