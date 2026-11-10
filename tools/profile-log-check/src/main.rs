//! Fixture: summarizes Echo profile logs (warnings, errors, slow layout/pane/emote
//! stages, scroll and refresh counters) as one CSV row per log.
//! Usage: `profile-log-check <profile.log> [...]`.

use std::env;
use std::fs;
use std::path::Path;

#[derive(Debug, Default)]
struct LogSummary {
    source: String,
    warn_count: u64,
    error_count: u64,
    panic_count: u64,
    fatal_count: u64,
    failed_count: u64,
    slow_layout_count: u64,
    slow_layout_max_ms: f64,
    slow_layout_fresh_total: u64,
    slow_layout_reused_total: u64,
    slow_pane_count: u64,
    slow_pane_max_ms: f64,
    slow_visible_asset_refresh_count: u64,
    slow_visible_asset_refresh_max_ms: f64,
    slow_emote_stage_count: u64,
    slow_emote_stage_max_ms: f64,
    top_slow_emote_stage: String,
    top_slow_emote_stage_ms: f64,
    slow_cache_preload_stage_count: u64,
    slow_cache_preload_stage_max_ms: f64,
    top_slow_cache_preload_stage: String,
    top_slow_cache_preload_source: String,
    top_slow_cache_preload_stage_ms: f64,
    scroll_smoke_step_count: u64,
    canvas_scroll_by_pixels_count: u64,
    canvas_scroll_wheel_event_count: u64,
    canvas_scroll_wheel_notify_count: u64,
    max_viewport_canvas_prepared_row_reuse: u64,
    max_viewport_canvas_scroll_snapshot_reuse: u64,
    max_viewport_canvas_prepared_text_lines: u64,
    max_viewport_canvas_prepared_static_images: u64,
    post_scroll_full_rebuilds_total: u64,
    post_scroll_order_resets_total: u64,
    post_scroll_max_viewport_canvas_prepared_row_reuse: u64,
    post_scroll_max_viewport_canvas_scroll_snapshot_reuse: u64,
    refreshing_all_count: u64,
    uncached_refresh_count: u64,
    root_chat_backend_messages_updated_notify_count: u64,
    stale_layout_refresh_count: u64,
    max_stale_layout_refresh_work_items: u64,
    stale_layout_refresh_sync_visible_rebuilt_total: u64,
    stale_layout_refresh_async_requested_total: u64,
    stale_layout_refresh_heights_updated_total: u64,
    stale_layout_refresh_wrapped_count: u64,
    stale_layout_refresh_stale_remaining_count: u64,
    stale_layout_refresh_cleared_count: u64,
    highlight_color_changed_event_count: u64,
    global_layout_cache_settings_invalidation_count: u64,
    shutdown_summary_present: bool,
    summary_written: bool,
}

fn main() {
    let paths = env::args().skip(1).collect::<Vec<_>>();
    if paths.is_empty() {
        eprintln!("usage: profile-log-check <profile.log> [...]");
        std::process::exit(2);
    }

    println!(
        "source,warn_count,error_count,panic_count,fatal_count,failed_count,slow_layout_count,slow_layout_max_ms,slow_layout_fresh_total,slow_layout_reused_total,slow_pane_count,slow_pane_max_ms,slow_visible_asset_refresh_count,slow_visible_asset_refresh_max_ms,slow_emote_stage_count,slow_emote_stage_max_ms,top_slow_emote_stage,top_slow_emote_stage_ms,slow_cache_preload_stage_count,slow_cache_preload_stage_max_ms,top_slow_cache_preload_stage,top_slow_cache_preload_source,top_slow_cache_preload_stage_ms,scroll_smoke_step_count,canvas_scroll_by_pixels_count,canvas_scroll_wheel_event_count,canvas_scroll_wheel_notify_count,max_viewport_canvas_prepared_row_reuse,max_viewport_canvas_scroll_snapshot_reuse,max_viewport_canvas_prepared_text_lines,max_viewport_canvas_prepared_static_images,post_scroll_full_rebuilds_total,post_scroll_order_resets_total,post_scroll_max_viewport_canvas_prepared_row_reuse,post_scroll_max_viewport_canvas_scroll_snapshot_reuse,refreshing_all_count,uncached_refresh_count,root_chat_backend_messages_updated_notify_count,stale_layout_refresh_count,max_stale_layout_refresh_work_items,stale_layout_refresh_sync_visible_rebuilt_total,stale_layout_refresh_async_requested_total,stale_layout_refresh_heights_updated_total,stale_layout_refresh_wrapped_count,stale_layout_refresh_stale_remaining_count,stale_layout_refresh_cleared_count,highlight_color_changed_event_count,global_layout_cache_settings_invalidation_count,shutdown_summary_present,summary_written"
    );

    for path in paths {
        match summarize_log(&path) {
            Ok(summary) => println!("{}", summary.to_csv_row()),
            Err(error) => {
                eprintln!("{path}: {error}");
                std::process::exit(1);
            }
        }
    }
}

impl LogSummary {
    fn to_csv_row(&self) -> String {
        [
            csv_field(&self.source),
            self.warn_count.to_string(),
            self.error_count.to_string(),
            self.panic_count.to_string(),
            self.fatal_count.to_string(),
            self.failed_count.to_string(),
            self.slow_layout_count.to_string(),
            format!("{:.6}", self.slow_layout_max_ms),
            self.slow_layout_fresh_total.to_string(),
            self.slow_layout_reused_total.to_string(),
            self.slow_pane_count.to_string(),
            format!("{:.6}", self.slow_pane_max_ms),
            self.slow_visible_asset_refresh_count.to_string(),
            format!("{:.6}", self.slow_visible_asset_refresh_max_ms),
            self.slow_emote_stage_count.to_string(),
            format!("{:.6}", self.slow_emote_stage_max_ms),
            csv_field(&self.top_slow_emote_stage),
            format!("{:.6}", self.top_slow_emote_stage_ms),
            self.slow_cache_preload_stage_count.to_string(),
            format!("{:.6}", self.slow_cache_preload_stage_max_ms),
            csv_field(&self.top_slow_cache_preload_stage),
            csv_field(&self.top_slow_cache_preload_source),
            format!("{:.6}", self.top_slow_cache_preload_stage_ms),
            self.scroll_smoke_step_count.to_string(),
            self.canvas_scroll_by_pixels_count.to_string(),
            self.canvas_scroll_wheel_event_count.to_string(),
            self.canvas_scroll_wheel_notify_count.to_string(),
            self.max_viewport_canvas_prepared_row_reuse.to_string(),
            self.max_viewport_canvas_scroll_snapshot_reuse.to_string(),
            self.max_viewport_canvas_prepared_text_lines.to_string(),
            self.max_viewport_canvas_prepared_static_images.to_string(),
            self.post_scroll_full_rebuilds_total.to_string(),
            self.post_scroll_order_resets_total.to_string(),
            self.post_scroll_max_viewport_canvas_prepared_row_reuse
                .to_string(),
            self.post_scroll_max_viewport_canvas_scroll_snapshot_reuse
                .to_string(),
            self.refreshing_all_count.to_string(),
            self.uncached_refresh_count.to_string(),
            self.root_chat_backend_messages_updated_notify_count
                .to_string(),
            self.stale_layout_refresh_count.to_string(),
            self.max_stale_layout_refresh_work_items.to_string(),
            self.stale_layout_refresh_sync_visible_rebuilt_total
                .to_string(),
            self.stale_layout_refresh_async_requested_total.to_string(),
            self.stale_layout_refresh_heights_updated_total.to_string(),
            self.stale_layout_refresh_wrapped_count.to_string(),
            self.stale_layout_refresh_stale_remaining_count.to_string(),
            self.stale_layout_refresh_cleared_count.to_string(),
            self.highlight_color_changed_event_count.to_string(),
            self.global_layout_cache_settings_invalidation_count
                .to_string(),
            self.shutdown_summary_present.to_string(),
            self.summary_written.to_string(),
        ]
        .join(",")
    }
}

fn summarize_log(path: &str) -> Result<LogSummary, String> {
    let content = fs::read_to_string(path).map_err(|error| format!("failed to read: {error}"))?;
    let mut summary = LogSummary {
        source: Path::new(path)
            .to_str()
            .ok_or_else(|| "profile path is not valid UTF-8".to_string())?
            .to_string(),
        ..LogSummary::default()
    };
    let mut seen_scroll_input = false;

    for line in content.lines() {
        let line = strip_ansi(line);
        let lower = line.to_ascii_lowercase();
        let is_warning = has_log_level(&line, "WARN");
        let is_error = has_log_level(&line, "ERROR");
        let is_failure_level = is_warning || is_error;

        if is_warning {
            summary.warn_count += 1;
        }
        if is_error {
            summary.error_count += 1;
        }
        if contains_panic_signal(&lower, is_failure_level) {
            summary.panic_count += 1;
        }
        if contains_fatal_signal(&lower, is_failure_level) {
            summary.fatal_count += 1;
        }
        if is_failure_level && contains_failed_signal(&lower) {
            summary.failed_count += 1;
        }
        if line.contains("bench.scroll_smoke step=") {
            summary.scroll_smoke_step_count += 1;
            seen_scroll_input = true;
        }
        if line.contains("canvas.scroll_by_pixels") {
            summary.canvas_scroll_by_pixels_count += 1;
            seen_scroll_input = true;
        }
        if line.contains("canvas.scroll_wheel_event") {
            summary.canvas_scroll_wheel_event_count += 1;
            seen_scroll_input = true;
        }
        if line.contains("canvas.scroll_wheel") && !line.contains("canvas.scroll_wheel_event") {
            summary.canvas_scroll_wheel_notify_count += 1;
        }
        if lower.contains("refreshing all") {
            summary.refreshing_all_count += 1;
        }
        if line.contains("uncached_refresh=true") {
            summary.uncached_refresh_count += 1;
        }
        if line.contains("app.chat_backend.messages_updated") {
            summary.root_chat_backend_messages_updated_notify_count += 1;
        }
        if line.contains("HighlightColorChanged") {
            summary.highlight_color_changed_event_count += 1;
        }
        if line.contains("Global layout cache invalidated for settings change") {
            summary.global_layout_cache_settings_invalidation_count += 1;
        }

        if line.contains("Layout metrics breakdown") {
            summary.slow_layout_count += 1;
            update_max(
                &mut summary.slow_layout_max_ms,
                f64_field(&line, "total_ms"),
            );
            summary.slow_layout_fresh_total += u64_field(&line, "fresh").unwrap_or(0);
            summary.slow_layout_reused_total += u64_field(&line, "reused").unwrap_or(0);
        }

        if line.contains("Slow pane view build") {
            summary.slow_pane_count += 1;
            update_max(
                &mut summary.slow_pane_max_ms,
                f64_field(&line, "elapsed_ms"),
            );
        }

        if line.contains("Slow visible asset refresh") {
            summary.slow_visible_asset_refresh_count += 1;
            update_max(
                &mut summary.slow_visible_asset_refresh_max_ms,
                f64_field(&line, "elapsed_ms"),
            );
        }

        if line.contains("Slow emote pipeline stage") {
            summary.slow_emote_stage_count += 1;
            let elapsed_ms = f64_field(&line, "elapsed_ms").unwrap_or(0.0);
            update_max(&mut summary.slow_emote_stage_max_ms, Some(elapsed_ms));
            if elapsed_ms >= summary.top_slow_emote_stage_ms {
                summary.top_slow_emote_stage_ms = elapsed_ms;
                summary.top_slow_emote_stage = string_field(&line, "stage").unwrap_or_default();
            }
        }

        if line.contains("Slow cache preload stage") {
            summary.slow_cache_preload_stage_count += 1;
            let elapsed_ms = f64_field(&line, "elapsed_ms").unwrap_or(0.0);
            update_max(
                &mut summary.slow_cache_preload_stage_max_ms,
                Some(elapsed_ms),
            );
            if elapsed_ms >= summary.top_slow_cache_preload_stage_ms {
                summary.top_slow_cache_preload_stage_ms = elapsed_ms;
                summary.top_slow_cache_preload_stage =
                    string_field(&line, "stage").unwrap_or_default();
                summary.top_slow_cache_preload_source =
                    string_field(&line, "source").unwrap_or_default();
            }
        }

        if line.contains("Canvas render/layout aggregate") {
            update_max_u64(
                &mut summary.max_viewport_canvas_prepared_row_reuse,
                u64_field(&line, "viewport_canvas_prepared_row_reuse"),
            );
            update_max_u64(
                &mut summary.max_viewport_canvas_scroll_snapshot_reuse,
                u64_field(&line, "viewport_canvas_scroll_snapshot_reuse"),
            );
            update_max_u64(
                &mut summary.max_viewport_canvas_prepared_text_lines,
                u64_field(&line, "viewport_canvas_prepared_text_lines"),
            );
            update_max_u64(
                &mut summary.max_viewport_canvas_prepared_static_images,
                u64_field(&line, "viewport_canvas_prepared_static_images"),
            );

            if seen_scroll_input {
                summary.post_scroll_full_rebuilds_total +=
                    u64_field(&line, "full_rebuilds").unwrap_or(0);
                summary.post_scroll_order_resets_total +=
                    u64_field(&line, "order_resets").unwrap_or(0);
                update_max_u64(
                    &mut summary.post_scroll_max_viewport_canvas_prepared_row_reuse,
                    u64_field(&line, "viewport_canvas_prepared_row_reuse"),
                );
                update_max_u64(
                    &mut summary.post_scroll_max_viewport_canvas_scroll_snapshot_reuse,
                    u64_field(&line, "viewport_canvas_scroll_snapshot_reuse"),
                );
            }
        }
        if line.contains("Incremental stale-layout refresh") {
            summary.stale_layout_refresh_count += 1;
            update_max_u64(
                &mut summary.max_stale_layout_refresh_work_items,
                u64_field(&line, "work_items"),
            );
            summary.stale_layout_refresh_sync_visible_rebuilt_total +=
                u64_field(&line, "sync_visible_rebuilt").unwrap_or(0);
            summary.stale_layout_refresh_async_requested_total +=
                u64_field(&line, "async_requested").unwrap_or(0);
            summary.stale_layout_refresh_heights_updated_total +=
                u64_field(&line, "heights_updated").unwrap_or(0);

            let wrapped = bool_field(&line, "wrapped").unwrap_or(false);
            let stale_remaining = bool_field(&line, "stale_remaining").unwrap_or(false);
            if wrapped {
                summary.stale_layout_refresh_wrapped_count += 1;
            }
            if stale_remaining {
                summary.stale_layout_refresh_stale_remaining_count += 1;
            } else if wrapped {
                summary.stale_layout_refresh_cleared_count += 1;
            }
        }

        if line.contains("Performance summary on shutdown") {
            summary.shutdown_summary_present = true;
        }
        if line.contains("Wrote profiling summary") {
            summary.summary_written = true;
        }
    }

    Ok(summary)
}

fn has_log_level(line: &str, level: &str) -> bool {
    line.contains(&format!(" {level} ")) || line.contains(&format!("[{level}]"))
}

fn contains_token(source: &str, token: &str) -> bool {
    source
        .split(|ch: char| !ch.is_ascii_alphanumeric() && ch != '_')
        .any(|part| part == token)
}

fn contains_panic_signal(source: &str, is_failure_level: bool) -> bool {
    (is_failure_level && (contains_token(source, "panic") || contains_token(source, "panicked")))
        || (source.contains("thread '") && source.contains(" panicked at "))
}

fn contains_fatal_signal(source: &str, is_failure_level: bool) -> bool {
    (is_failure_level && contains_token(source, "fatal"))
        || source.trim_start().starts_with("fatal runtime error:")
}

fn contains_failed_signal(source: &str) -> bool {
    source.contains(" failed ")
        || source.contains(" failed:")
        || source.contains(": failed")
        || source.contains("failed to ")
}

fn update_max(target: &mut f64, candidate: Option<f64>) {
    if let Some(value) = candidate
        && value > *target
    {
        *target = value;
    }
}

fn update_max_u64(target: &mut u64, candidate: Option<u64>) {
    if let Some(value) = candidate
        && value > *target
    {
        *target = value;
    }
}

fn f64_field(source: &str, name: &str) -> Option<f64> {
    field_value(source, name)?.parse::<f64>().ok()
}

fn u64_field(source: &str, name: &str) -> Option<u64> {
    field_value(source, name)?.parse::<u64>().ok()
}

fn bool_field(source: &str, name: &str) -> Option<bool> {
    field_value(source, name)?.parse::<bool>().ok()
}

fn string_field(source: &str, name: &str) -> Option<String> {
    field_value(source, name)
}

fn field_value(source: &str, name: &str) -> Option<String> {
    let needle = format!("{name}=");
    let start = source.find(&needle)? + needle.len();
    let value = source[start..].trim_start();
    if let Some(value) = value.strip_prefix('"') {
        let end = value.find('"')?;
        return Some(value[..end].to_string());
    }

    let end = value
        .find(|ch: char| ch.is_ascii_whitespace() || ch == ',' || ch == '}')
        .unwrap_or(value.len());
    Some(value[..end].trim_matches('"').to_string())
}

fn strip_ansi(input: &str) -> String {
    let mut output = String::with_capacity(input.len());
    let mut chars = input.chars();
    while let Some(ch) = chars.next() {
        if ch == '\x1b' {
            if matches!(chars.next(), Some('[')) {
                for next in chars.by_ref() {
                    if ('@'..='~').contains(&next) {
                        break;
                    }
                }
            }
        } else {
            output.push(ch);
        }
    }
    output
}

fn csv_field(value: &str) -> String {
    if value.contains(',') || value.contains('"') || value.contains('\n') {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.to_string()
    }
}
