//! Debug settings section
//!
//! Developer-facing tools and diagnostics with real-time performance metrics.

use crate::backend;
use crate::components::settings::settings_navigation::SettingDescriptor;
use crate::components::settings::widgets::{SettingsMetrics, setting_target, settings_subheading};
use crate::components::ui::ButtonExt;
use crate::diagnostics::fps_profiler;
use crate::theme::colors;
use crate::theme::tokens::{RadiiPixels, Spacing};
use gpui::prelude::FluentBuilder;
use gpui::*;

pub(super) const FRAME_RATE_ANALYSIS: SettingDescriptor = SettingDescriptor {
    id: "developer-tools.frame-rate-analysis",
    label: "Frame rate",
    aliases: &["Frame Rate Analysis", "fps", "jank", "p95", "frame time"],
    control_id: "debug-frame-rate",
    disclosures: &["advanced-diagnostics"],
};

pub(super) const RUNTIME_LIVENESS: SettingDescriptor = SettingDescriptor {
    id: "developer-tools.runtime-liveness",
    label: "Runtime liveness",
    aliases: &[
        "Runtime Liveness",
        "idle",
        "focus",
        "frame skipping",
        "activity",
    ],
    control_id: "debug-runtime-liveness",
    disclosures: &["advanced-diagnostics"],
};

pub(super) const SEVENTV_EVENTAPI: SettingDescriptor = SettingDescriptor {
    id: "developer-tools.seventv-eventapi",
    label: "7TV EventAPI",
    aliases: &["websocket", "subscription", "heartbeat", "reconnect"],
    control_id: "debug-seventv-eventapi",
    disclosures: &["advanced-diagnostics"],
};

pub(super) const MESSAGE_PERFORMANCE: SettingDescriptor = SettingDescriptor {
    id: "developer-tools.message-performance",
    label: "Message performance",
    aliases: &[
        "Message Performance",
        "throughput",
        "batch",
        "messages per second",
    ],
    control_id: "debug-message-performance",
    disclosures: &["advanced-diagnostics"],
};

pub(super) const EMOTE_LOADING: SettingDescriptor = SettingDescriptor {
    id: "developer-tools.emote-loading",
    label: "Emote loading",
    aliases: &["Emote Loading", "emote failures", "success rate", "loads"],
    control_id: "debug-emote-loading",
    disclosures: &["advanced-diagnostics"],
};

pub(super) const MEMORY_USAGE: SettingDescriptor = SettingDescriptor {
    id: "developer-tools.memory-usage",
    label: "Memory",
    aliases: &["Memory Usage", "memory", "buffer", "stored messages"],
    control_id: "debug-memory-usage",
    disclosures: &["advanced-diagnostics"],
};

pub(super) const DEBUG_ACTIONS: SettingDescriptor = SettingDescriptor {
    id: "developer-tools.debug-actions",
    label: "Diagnostics actions",
    aliases: &[
        "fps overlay",
        "copy report",
        "reset metrics",
        "crash reports",
        "crash",
    ],
    control_id: "debug-actions",
    disclosures: &["advanced-diagnostics"],
};

/// Render the body of Advanced › Developer diagnostics.
pub fn render_debug_section<T: 'static>(cx: &mut Context<T>) -> impl IntoElement + use<T> {
    render_debug_section_content(cx)
}

/// Render the diagnostics body for live-refreshing popup surfaces.
pub fn render_debug_section_live<T: 'static>(cx: &mut Context<T>) -> impl IntoElement + use<T> {
    render_debug_section(cx)
}

fn render_debug_section_content<T: 'static>(cx: &mut Context<T>) -> impl IntoElement + use<T> {
    let metrics = backend::metrics::get_summary();
    let memory = backend::metrics::get_memory_stats();
    let fps_stats = fps_profiler::get_stats();
    let frame_diag = crate::managers::frame_rate_controller::diagnostics();
    let eventapi_diag = backend::emotes::seventv_eventapi::diagnostics();

    div()
        .w_full()
        .min_w_0()
        .flex()
        .flex_col()
        .gap(px(SettingsMetrics::GROUP_GAP))
        .child(render_health_summary(
            fps_stats.fps,
            eventapi_diag.connected,
            memory.message_buffer_bytes,
        ))
        .child(render_metric_group(
            FRAME_RATE_ANALYSIS,
            div()
                .flex()
                .flex_col()
                .child(render_metric_row("FPS", format!("{:.1}", fps_stats.fps)))
                .child(render_metric_row(
                    "Average frame",
                    format!("{:.2}ms", fps_stats.avg_ms),
                ))
                .child(render_metric_row(
                    "p50 frame",
                    format!("{:.2}ms", fps_stats.p50_ms),
                ))
                .child(render_metric_row(
                    "p95 frame",
                    format!("{:.2}ms", fps_stats.p95_ms),
                ))
                .child(render_metric_row(
                    "p99 frame",
                    format!("{:.2}ms", fps_stats.p99_ms),
                ))
                .child(render_metric_row(
                    "Jank",
                    format!("{:.1}%", fps_stats.jank_percent),
                ))
                .child(render_metric_row(
                    "Variance",
                    format!("{:.2}ms", fps_stats.variance_ms),
                ))
                .child(render_metric_row(
                    "Total frames",
                    format!("{}", fps_stats.total_frames),
                )),
        ))
        .child(render_metric_group(
            RUNTIME_LIVENESS,
            div()
                .flex()
                .flex_col()
                .child(render_metric_row("Mode", format!("{:?}", frame_diag.mode)))
                .child(render_metric_row("Focused", yes_no(frame_diag.focused)))
                .child(render_metric_row(
                    "Skipping frames",
                    yes_no(frame_diag.skipping_frames),
                ))
                .child(render_metric_row(
                    "Visible animations",
                    yes_no(frame_diag.visible_animations),
                ))
                .child(render_metric_row(
                    "Last activity",
                    format_age_ms(frame_diag.last_activity_ms_ago),
                ))
                .child(render_metric_row(
                    "Last IRC message",
                    format_age_ms(frame_diag.last_irc_message_ms_ago),
                ))
                .child(render_metric_row(
                    "Last observer drain",
                    format_age_ms(frame_diag.last_observer_drain_ms_ago),
                ))
                .child(render_metric_row(
                    "Last image ready",
                    format_age_ms(frame_diag.last_image_ready_ms_ago),
                )),
        ))
        .child(render_metric_group(
            SEVENTV_EVENTAPI,
            div()
                .flex()
                .flex_col()
                .child(render_metric_row(
                    "Connected",
                    yes_no(eventapi_diag.connected),
                ))
                .child(render_metric_row(
                    "Subscriptions",
                    format!(
                        "{} desired / {} ACKed / {} in flight / {} queued",
                        eventapi_diag.desired_subscriptions,
                        eventapi_diag.active_subscriptions,
                        eventapi_diag.in_flight_subscription_commands,
                        eventapi_diag.queued_subscription_commands
                    ),
                ))
                .child(render_metric_row(
                    "Ownership",
                    format!(
                        "{} channels / {} sets / {} users",
                        eventapi_diag.owned_channels,
                        eventapi_diag.owned_sets,
                        eventapi_diag.owned_users
                    ),
                ))
                .child(render_metric_row(
                    "Pending refreshes",
                    format!(
                        "{} set patches / {} active-set fetches",
                        eventapi_diag.pending_refreshes, eventapi_diag.active_set_refreshes
                    ),
                ))
                .child(render_metric_row(
                    "Heartbeat age",
                    format_age_ms(eventapi_diag.last_heartbeat_ms_ago),
                ))
                .child(render_metric_row(
                    "Reconnects",
                    eventapi_diag.reconnects.to_string(),
                ))
                .child(render_metric_row(
                    "Last error",
                    eventapi_diag
                        .last_error
                        .unwrap_or_else(|| "none".to_string()),
                )),
        ))
        .child(render_metric_group(
            MESSAGE_PERFORMANCE,
            div()
                .flex()
                .flex_col()
                .child(render_metric_row(
                    "Messages per second",
                    format!("{:.1}", metrics.messages_per_second),
                ))
                .child(render_metric_row(
                    "Average batch time",
                    format!("{:.2}ms", metrics.avg_batch_time_ms),
                ))
                .child(render_metric_row(
                    "Messages received",
                    metrics.messages_received.to_string(),
                ))
                .child(render_metric_row(
                    "Messages rendered",
                    metrics.messages_rendered.to_string(),
                )),
        ))
        .child(render_metric_group(
            EMOTE_LOADING,
            div()
                .flex()
                .flex_col()
                .child(render_metric_row(
                    "Emotes loaded",
                    metrics.emote_loads.to_string(),
                ))
                .child(render_metric_row(
                    "Emote failures",
                    metrics.emote_failures.to_string(),
                ))
                .child(render_metric_row(
                    "Success rate",
                    format!("{:.1}%", metrics.emote_success_rate),
                )),
        ))
        .child(render_metric_group(
            MEMORY_USAGE,
            div()
                .flex()
                .flex_col()
                .child(render_metric_row(
                    "Active channels",
                    memory.active_channels.to_string(),
                ))
                .child(render_metric_row(
                    "Stored messages",
                    memory.stored_messages.to_string(),
                ))
                .child(render_metric_row(
                    "Estimated memory",
                    format!("{:.1} MB", memory.message_buffer_bytes as f64 / 1_048_576.0),
                )),
        ))
        .child(render_diagnostics_actions(cx))
}

fn render_health_summary(
    fps: f64,
    eventapi_connected: bool,
    memory_bytes: u64,
) -> impl IntoElement {
    let eventapi = if eventapi_connected {
        "7TV connected"
    } else {
        "7TV disconnected"
    };

    div()
        .id("developer-diagnostics-health")
        .role(Role::Group)
        .aria_label("Diagnostics health summary")
        .w_full()
        .min_w_0()
        .flex()
        .flex_wrap()
        .items_center()
        .gap_x(px(Spacing::MD))
        .gap_y(px(Spacing::XS))
        .py(px(Spacing::XS_PLUS))
        .border_b_1()
        .border_color(colors::settings_group_rule(colors::bg_surface()))
        .child(
            div()
                .text_size(px(SettingsMetrics::ROW_LABEL_TEXT))
                .font_weight(FontWeight::MEDIUM)
                .text_color(colors::text_primary())
                .child("Health summary"),
        )
        .child(
            div()
                .text_size(px(SettingsMetrics::ROW_HELPER_TEXT))
                .text_color(colors::text_secondary())
                .child(format!(
                    "{fps:.1} FPS · {eventapi} · {:.1} MB",
                    memory_bytes as f64 / 1_048_576.0
                )),
        )
}

fn render_metric_group<C: IntoElement>(
    descriptor: SettingDescriptor,
    rows: C,
) -> impl IntoElement + use<C> {
    setting_target(
        descriptor,
        div()
            .w_full()
            .min_w_0()
            .flex()
            .flex_col()
            .gap(px(Spacing::XS_PLUS))
            .child(settings_subheading(descriptor.label))
            .child(rows),
    )
}

fn yes_no(value: bool) -> String {
    if value {
        "yes".to_string()
    } else {
        "no".to_string()
    }
}

fn format_age_ms(value: Option<u64>) -> String {
    match value {
        Some(ms) if ms < 1_000 => format!("{}ms ago", ms),
        Some(ms) => format!("{:.1}s ago", ms as f64 / 1_000.0),
        None => "n/a".to_string(),
    }
}

/// Compact metric rows wrap their value below the label at narrow widths.
fn render_metric_row(label: &'static str, value: String) -> impl IntoElement + use<> {
    div()
        .w_full()
        .min_w_0()
        .py(px(Spacing::XS_PLUS))
        .flex()
        .flex_wrap()
        .items_center()
        .justify_between()
        .gap_x(px(Spacing::MD))
        .gap_y(px(Spacing::XS))
        .border_b_1()
        .border_color(colors::settings_group_rule(colors::bg_surface()))
        .child(
            div()
                .min_w_0()
                .flex_1()
                .flex_basis(px(SettingsMetrics::ROW_CONTROL_WIDTH))
                .text_size(px(SettingsMetrics::ROW_HELPER_TEXT))
                .text_color(colors::text_secondary())
                .child(label),
        )
        .child(
            div()
                .min_w_0()
                .max_w_full()
                .text_size(px(SettingsMetrics::ROW_HELPER_TEXT))
                .font_weight(FontWeight::MEDIUM)
                .text_color(colors::text_primary())
                .whitespace_normal()
                .child(value),
        )
}

fn render_debug_action_row<T: 'static, F: Fn(&mut App) + 'static>(
    title: &'static str,
    description: &'static str,
    button_label: &'static str,
    on_click: F,
    cx: &mut Context<T>,
) -> impl IntoElement + use<T, F> {
    div()
        .w_full()
        .min_w_0()
        .py(px(Spacing::XS_PLUS))
        .flex()
        .flex_wrap()
        .items_center()
        .justify_between()
        .gap_x(px(Spacing::MD))
        .gap_y(px(Spacing::XS_PLUS))
        .border_b_1()
        .border_color(colors::settings_group_rule(colors::bg_surface()))
        .child(
            div()
                .min_w_0()
                .flex_1()
                .flex()
                .flex_col()
                .gap(px(Spacing::XXS))
                .child(
                    div()
                        .text_size(px(SettingsMetrics::ROW_LABEL_TEXT))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(colors::text_primary())
                        .child(title),
                )
                .child(
                    div()
                        .min_w_0()
                        .text_size(px(SettingsMetrics::ROW_HELPER_TEXT))
                        .text_color(colors::text_secondary())
                        .whitespace_normal()
                        .child(description),
                ),
        )
        .child(
            div()
                .id(SharedString::from(format!(
                    "debug-action-{}",
                    crate::components::settings::widgets::control_slug(title)
                )))
                .role(Role::Button)
                .map(|control| {
                    crate::components::settings::widgets::settings_keyboard_control(control, cx)
                })
                .a11y_synthetic_children(crate::components::ui::accessibility::apply_description)
                .aria_label(SharedString::from(format!("{title}: {button_label}")))
                .aria_description(SharedString::from(description))
                .px(px(SettingsMetrics::COMPACT_BUTTON_PADDING_X))
                .py(px(SettingsMetrics::COMPACT_BUTTON_PADDING_Y))
                .min_h(px(SettingsMetrics::CONTROL_MIN_HEIGHT))
                .rounded(crate::theme::tokens::Radii::sm())
                .bg(colors::bg_surface())
                .border_1()
                .border_color(colors::border())
                .cursor_pointer()
                .hover(|style| style.bg(colors::bg_hover()))
                .on_accessible_activate(move |_, _, cx| on_click(cx))
                .child(
                    div()
                        .text_size(px(SettingsMetrics::ROW_HELPER_TEXT))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(colors::interactive_foreground())
                        .child(button_label),
                ),
        )
}

fn render_diagnostics_actions<T: 'static>(cx: &mut Context<T>) -> impl IntoElement + use<T> {
    setting_target(
        DEBUG_ACTIONS,
        div()
            .w_full()
            .min_w_0()
            .flex()
            .flex_col()
            .gap(px(Spacing::XS_PLUS))
            .child(settings_subheading("Actions"))
            .child(render_fps_overlay_toggle(cx))
            .child(render_copy_fps_report_button(cx))
            .child(setting_target(
                super::logging::DIAGNOSTICS_JSON,
                render_copy_diagnostics_json_button(cx),
            ))
            .child(setting_target(
                super::logging::DIAGNOSTICS_RESET,
                render_reset_metrics_button(cx),
            ))
            .child(render_open_crash_reports_button(cx))
            .child(render_copy_eventapi_report_button(cx)),
    )
}

fn render_copy_eventapi_report_button<T: 'static>(
    cx: &mut Context<T>,
) -> impl IntoElement + use<T> {
    render_debug_action_row(
        "Copy 7TV diagnostics",
        "Copy EventAPI ownership and liveness snapshot",
        "Copy",
        |cx| {
            cx.write_to_clipboard(ClipboardItem::new_string(eventapi_report()));
        },
        cx,
    )
}

fn eventapi_report() -> String {
    let diag = backend::emotes::seventv_eventapi::diagnostics();
    let ownership = backend::emotes::seventv_eventapi::ownership_snapshot();
    let mut report = String::new();

    report.push_str("7TV EventAPI diagnostics\n");
    report.push_str(&format!(
        "connected={} desired={} acked={} in_flight={} queued={} reconnects={} heartbeat={} pending_refreshes={} active_set_refreshes={} last_error={}\n\n",
        diag.connected,
        diag.desired_subscriptions,
        diag.active_subscriptions,
        diag.in_flight_subscription_commands,
        diag.queued_subscription_commands,
        diag.reconnects,
        format_age_ms(diag.last_heartbeat_ms_ago),
        diag.pending_refreshes,
        diag.active_set_refreshes,
        diag.last_error.unwrap_or_else(|| "none".to_string())
    ));

    report.push_str("channels\n");
    for row in ownership.channels {
        report.push_str(&format!(
            "- {} set={} user={} connection={}\n",
            row.channel,
            row.emote_set_id,
            row.user_id.unwrap_or_else(|| "none".to_string()),
            row.connection_index
                .map(|value| value.to_string())
                .unwrap_or_else(|| "n/a".to_string())
        ));
    }

    report.push_str("\nsets\n");
    for row in ownership.sets {
        report.push_str(&format!(
            "- {} channels={}\n",
            row.emote_set_id,
            row.channels.join(",")
        ));
    }

    report.push_str("\nusers\n");
    for row in ownership.users {
        report.push_str(&format!(
            "- {} channels={}\n",
            row.user_id,
            row.channels.join(",")
        ));
    }

    report.push_str("\nactive set refreshes\n");
    for (channel, set_id) in ownership.active_set_refreshes {
        report.push_str(&format!("- {} -> {}\n", channel, set_id));
    }

    report
}

/// Render the Reset Metrics button
fn render_reset_metrics_button<T: 'static>(cx: &mut Context<T>) -> impl IntoElement + use<T> {
    render_debug_action_row(
        "Reset metrics",
        "Clear all performance counters",
        "Run",
        |_| {
            backend::reset_metrics();
        },
        cx,
    )
}

/// Render the Copy diagnostics JSON button.
fn render_copy_diagnostics_json_button<T: 'static>(
    cx: &mut Context<T>,
) -> impl IntoElement + use<T> {
    render_debug_action_row(
        "Copy diagnostics JSON",
        "Copy runtime metrics and diagnostic counters to the clipboard",
        "Copy",
        |cx| {
            let json = backend::get_metrics_json();
            cx.write_to_clipboard(ClipboardItem::new_string(json));
        },
        cx,
    )
}

fn render_open_crash_reports_button<T: 'static>(cx: &mut Context<T>) -> impl IntoElement + use<T> {
    render_debug_action_row(
        "Open crash reports folder",
        "Show saved crash reports in the file browser",
        "Open",
        |_| {
            let Some(path) = backend::reliability::crash_reports_dir() else {
                backend::push_ui_action(backend::UiAction::ShowWarningToast {
                    message: "Crash reports folder unavailable".to_string(),
                    detail: None,
                });
                return;
            };
            if let Err(err) = std::fs::create_dir_all(&path) {
                backend::push_ui_action(backend::UiAction::ShowWarningToast {
                    message: "Could not create crash reports folder".to_string(),
                    detail: Some(format!("{}: {err}", path.display())),
                });
            } else {
                crate::utils::open_path(path.clone(), move |result| {
                    if let Err(err) = result {
                        backend::push_ui_action(backend::UiAction::ShowWarningToast {
                            message: "Could not open crash reports folder".to_string(),
                            detail: Some(format!("{}: {err}", path.display())),
                        });
                    }
                });
            }
        },
        cx,
    )
}

/// Render the FPS Overlay toggle button
fn render_fps_overlay_toggle<T: 'static>(cx: &mut Context<T>) -> impl IntoElement + use<T> {
    let is_enabled = fps_profiler::is_overlay_enabled();
    render_debug_action_row(
        "FPS overlay",
        "Show real-time FPS in top-left corner",
        if is_enabled { "ON" } else { "OFF" },
        move |cx| {
            fps_profiler::set_overlay_enabled(!is_enabled);
            cx.refresh_windows();
        },
        cx,
    )
}

/// Render the Copy FPS Report button
fn render_copy_fps_report_button<T: 'static>(cx: &mut Context<T>) -> impl IntoElement + use<T> {
    render_debug_action_row(
        "Copy FPS report",
        "Copy detailed frame analysis to clipboard",
        "Copy",
        |cx| {
            let stats = fps_profiler::get_stats();
            let report = stats.detailed_report();
            cx.write_to_clipboard(ClipboardItem::new_string(report));
        },
        cx,
    )
}
