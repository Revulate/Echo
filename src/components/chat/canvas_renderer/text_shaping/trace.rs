//! The `ECHO_LAYOUT_TRACE` guard around one layout build.

use super::*;

/// `(ECHO_LAYOUT_TRACE, ECHO_LAYOUT_TRACE_VERBOSE)` as `(enabled, verbose)`.
pub(super) static LAYOUT_TRACE_SWITCHES: LazyLock<(bool, bool)> =
    LazyLock::new(|| trace_switches("ECHO_LAYOUT_TRACE", "ECHO_LAYOUT_TRACE_VERBOSE"));

thread_local! {
    static LAYOUT_BUILD_DEPTH: Cell<usize> = const { Cell::new(0) };
}

pub(super) struct LayoutTraceGuard {
    /// `None` when layout tracing is off.
    span: Option<TraceSpan>,
    verbose: bool,
    message_id: String,
    channel_id: String,
}

impl LayoutTraceGuard {
    pub(super) fn enter(
        msg: &ChatMessage,
        channel_id: &str,
        container_width: f32,
        show_timestamp: bool,
        uses_window_text_metrics: bool,
    ) -> Self {
        let (enabled, verbose) = *LAYOUT_TRACE_SWITCHES;
        if !enabled {
            return Self {
                span: None,
                verbose: false,
                message_id: String::new(),
                channel_id: String::new(),
            };
        }

        let span = TraceSpan::enter(&LAYOUT_BUILD_DEPTH);
        let depth = span.depth();
        if verbose {
            let content_chars = msg.content.chars().count();
            crate::log_perf_render!(
                debug,
                "layout.build.start depth={} msg={} channel={} slots={} chars={} reply={} width={:.1} timestamp={} window_metrics={}",
                depth,
                msg.id,
                channel_id,
                msg.slots.len(),
                content_chars,
                msg.reply.is_some(),
                container_width,
                show_timestamp,
                uses_window_text_metrics
            );
        }
        if depth > 1 {
            crate::log_perf_render!(
                warn,
                "layout.build.reentrant depth={} msg={} channel={}",
                depth,
                msg.id,
                channel_id
            );
        }

        Self {
            span: Some(span),
            verbose,
            message_id: msg.id.to_string(),
            channel_id: channel_id.to_string(),
        }
    }

    pub(super) fn finish(&mut self, layout: &CanvasMessageLayout) {
        let Some(span) = self.span.as_mut() else {
            return;
        };
        let Some(elapsed_ms) = span.finish() else {
            return;
        };
        if self.verbose {
            crate::log_perf_render!(
                debug,
                "layout.build.finish depth={} msg={} channel={} elapsed_ms={:.2} height={:.1} lines={} segments={} shaped_lines={} hitboxes={} animated={}",
                span.depth(),
                self.message_id,
                self.channel_id,
                elapsed_ms,
                layout.height,
                layout.line_count,
                layout.segments.len(),
                layout.shaped_lines.len(),
                layout.hitbox_regions.len(),
                layout.animated_emote_count
            );
        } else if elapsed_ms >= TRACE_SLOW_MS {
            crate::log_perf_render!(
                warn,
                "layout.build.slow depth={} msg={} channel={} elapsed_ms={:.2} height={:.1} lines={} segments={} shaped_lines={} hitboxes={} animated={}",
                span.depth(),
                self.message_id,
                self.channel_id,
                elapsed_ms,
                layout.height,
                layout.line_count,
                layout.segments.len(),
                layout.shaped_lines.len(),
                layout.hitbox_regions.len(),
                layout.animated_emote_count
            );
        }
    }
}

impl Drop for LayoutTraceGuard {
    fn drop(&mut self) {
        if let Some(span) = self.span.as_ref()
            && !span.is_finished()
        {
            crate::log_perf_render!(
                error,
                "layout.build.unfinished depth={} msg={} channel={} elapsed_ms={:.2}",
                span.depth(),
                self.message_id,
                self.channel_id,
                span.elapsed_ms()
            );
        }
    }
}
