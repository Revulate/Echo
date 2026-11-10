//! Render trace flags and render-depth guard.

use super::*;
use crate::components::chat::canvas_renderer::trace_span::{
    TRACE_SLOW_MS, TraceSpan, trace_switches,
};
use std::cell::Cell;

/// `(ECHO_RENDER_TRACE, ECHO_RENDER_TRACE_VERBOSE)` as `(enabled, verbose)`.
static RENDER_TRACE_SWITCHES: LazyLock<(bool, bool)> =
    LazyLock::new(|| trace_switches("ECHO_RENDER_TRACE", "ECHO_RENDER_TRACE_VERBOSE"));
pub(super) static RENDER_TRACE_ENABLED: LazyLock<bool> = LazyLock::new(|| RENDER_TRACE_SWITCHES.0);
pub(super) static SCROLL_SMOKE_TRACE_ENABLED: LazyLock<bool> =
    LazyLock::new(|| echo_core::env::flag("ECHO_BENCH_SCROLL_SMOKE"));

thread_local! {
    static CANVAS_RENDER_DEPTH: Cell<usize> = const { Cell::new(0) };
}

pub(super) struct CanvasRenderTraceGuard {
    /// `None` when render tracing is off.
    span: Option<TraceSpan>,
    verbose: bool,
    channel_id: String,
}

impl CanvasRenderTraceGuard {
    pub(super) fn enter(
        channel_id: &str,
        message_count: usize,
        container_width: f32,
        viewport_height: f32,
    ) -> Self {
        let (enabled, verbose) = *RENDER_TRACE_SWITCHES;
        if !enabled {
            return Self {
                span: None,
                verbose: false,
                channel_id: String::new(),
            };
        }

        let span = TraceSpan::enter(&CANVAS_RENDER_DEPTH);
        let depth = span.depth();
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
            span: Some(span),
            verbose,
            channel_id: channel_id.to_string(),
        }
    }

    pub(super) fn finish(&mut self) {
        let Some(span) = self.span.as_mut() else {
            return;
        };
        let Some(elapsed_ms) = span.finish() else {
            return;
        };
        if self.verbose {
            crate::log_perf_render!(
                debug,
                "canvas.render.finish depth={} channel={} elapsed_ms={:.2}",
                span.depth(),
                self.channel_id,
                elapsed_ms
            );
        } else if elapsed_ms >= TRACE_SLOW_MS {
            crate::log_perf_render!(
                warn,
                "canvas.render.slow depth={} channel={} elapsed_ms={:.2}",
                span.depth(),
                self.channel_id,
                elapsed_ms
            );
        }
    }
}

impl Drop for CanvasRenderTraceGuard {
    fn drop(&mut self) {
        if let Some(span) = self.span.as_ref()
            && !span.is_finished()
        {
            crate::log_perf_render!(
                error,
                "canvas.render.unfinished depth={} channel={} elapsed_ms={:.2}",
                span.depth(),
                self.channel_id,
                span.elapsed_ms()
            );
        }
    }
}
