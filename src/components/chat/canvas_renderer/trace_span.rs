//! Shared scaffolding for the layout and canvas-render trace guards.

use std::cell::Cell;
use std::thread::LocalKey;
use std::time::Instant;

/// A traced span at least this long is logged even without verbose tracing.
pub(crate) const TRACE_SLOW_MS: f64 = 8.0;

/// `(enabled, verbose)` for a trace switch pair: `name` turns tracing on,
/// `verbose_name` turns it on with a log line for every span.
pub(crate) fn trace_switches(name: &str, verbose_name: &str) -> (bool, bool) {
    let verbose = echo_core::env::flag(verbose_name);
    (verbose || echo_core::env::flag(name), verbose)
}

/// Nesting depth and timing of one traced call. Entering increments the
/// thread's depth counter; dropping decrements it.
pub(crate) struct TraceSpan {
    depth_counter: &'static LocalKey<Cell<usize>>,
    depth: usize,
    started_at: Instant,
    finished: bool,
}

impl TraceSpan {
    pub(crate) fn enter(depth_counter: &'static LocalKey<Cell<usize>>) -> Self {
        let depth = depth_counter.with(|depth| {
            let next = depth.get() + 1;
            depth.set(next);
            next
        });
        Self {
            depth_counter,
            depth,
            started_at: Instant::now(),
            finished: false,
        }
    }

    /// 1 for an outermost span; more means the traced call re-entered itself.
    pub(crate) fn depth(&self) -> usize {
        self.depth
    }

    pub(crate) fn elapsed_ms(&self) -> f64 {
        self.started_at.elapsed().as_secs_f64() * 1000.0
    }

    /// Mark the span finished and return its duration, or `None` if it was
    /// already finished.
    pub(crate) fn finish(&mut self) -> Option<f64> {
        if self.finished {
            return None;
        }
        self.finished = true;
        Some(self.elapsed_ms())
    }

    pub(crate) fn is_finished(&self) -> bool {
        self.finished
    }
}

impl Drop for TraceSpan {
    fn drop(&mut self) {
        self.depth_counter.with(|depth| {
            depth.set(depth.get().saturating_sub(1));
        });
    }
}
