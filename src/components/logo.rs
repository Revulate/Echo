//! Echo Logo Component
//!
//! Renders the animated Echo logo with connection status colors.
//! Uses SVG asset with currentColor, colored via text_color style.
//!
//! Note: GPUI doesn't support SVG gradients, so we use solid colors.

use std::cell::Cell;
use std::time::{Duration, Instant};

use gpui::*;

use crate::backend::ConnectionStatus;
use crate::managers::dynamic_tick_scheduler::aligned_tick_delay;
use crate::theme::colors;

/// Pulse re-render interval while the window is focused. A multiple of the
/// 20 ms animation grid, so pulse draws land on the same frames as emote
/// animation draws instead of adding frames of their own.
const PULSE_TICK_MS: u64 = 40;
/// Pulse re-render interval while the window is in the background.
const PULSE_TICK_INACTIVE_MS: u64 = 100;

thread_local! {
    /// The pulsing status (`true` = disconnected) and when its pulse began.
    static PULSE_START: Cell<Option<(bool, Instant)>> = const { Cell::new(None) };
    static PULSE_WAKE_PENDING: Cell<bool> = const { Cell::new(false) };
}

/// Get the color based on connection status
fn status_color(status: &ConnectionStatus) -> Hsla {
    match status {
        ConnectionStatus::Connected => colors::interactive_foreground(),
        ConnectionStatus::Reconnecting => colors::warning(),
        ConnectionStatus::Disconnected => colors::signal_red(),
    }
}

/// Render the Echo logo SVG with connection status-based coloring
///
/// The logo changes appearance based on connection status:
/// - Connected: Neutral with fade-in animation on load
/// - Reconnecting: Orange with pulse animation
/// - Disconnected: Red with fade animation
///
/// The pulses re-render the owning view from an aligned timer at
/// [`PULSE_TICK_MS`] rather than GPUI's per-frame animation request, which
/// redraws the whole window at the display refresh rate.
pub fn render_logo<V: 'static>(
    connection_status: &ConnectionStatus,
    size: f32,
    window: &Window,
    cx: &mut Context<V>,
) -> AnyElement {
    let color = status_color(connection_status);
    let logo = |id: &'static str| {
        div()
            .id(id)
            .size(px(size))
            .flex()
            .items_center()
            .justify_center()
            .child(
                svg()
                    .path("icons/echo-logo.svg")
                    .size(px(size))
                    .text_color(color),
            )
    };

    let (id, disconnected, period, min_opacity) = match connection_status {
        ConnectionStatus::Connected => {
            PULSE_START.with(|start| start.set(None));
            return logo("echo-logo")
                .with_animation(
                    "logo-fade-in",
                    Animation::new(Duration::from_millis(600)).with_easing(ease_out),
                    |el, delta| el.opacity(delta),
                )
                .into_any_element();
        }
        ConnectionStatus::Reconnecting => (
            "echo-logo-reconnecting",
            false,
            Duration::from_millis(2000),
            0.4,
        ),
        ConnectionStatus::Disconnected => (
            "echo-logo-disconnected",
            true,
            Duration::from_millis(1500),
            0.3,
        ),
    };

    let pulse = pulsating_between(min_opacity, 1.0);
    let phase = if cx.reduce_motion() {
        0.0
    } else {
        let started = PULSE_START.with(|start| match start.get() {
            Some((status, started)) if status == disconnected => started,
            _ => {
                let now = Instant::now();
                start.set(Some((disconnected, now)));
                now
            }
        });
        schedule_pulse_wake(window, cx);
        (started.elapsed().as_secs_f32() / period.as_secs_f32()).fract()
    };
    logo(id).opacity(pulse(phase)).into_any_element()
}

/// Re-render the view that owns the logo on the next pulse tick. At most one
/// wake is pending, however often the view renders in between.
fn schedule_pulse_wake<V: 'static>(window: &Window, cx: &mut Context<V>) {
    if PULSE_WAKE_PENDING.with(|pending| pending.replace(true)) {
        return;
    }
    let interval_ms = if window.is_window_active() {
        PULSE_TICK_MS
    } else {
        PULSE_TICK_INACTIVE_MS
    };
    let delay = aligned_tick_delay(interval_ms);
    cx.spawn(async move |this, cx| {
        cx.background_executor().timer(delay).await;
        PULSE_WAKE_PENDING.with(|pending| pending.set(false));
        this.update(cx, |_, cx| cx.notify()).ok();
    })
    .detach();
}

/// Easing function for smooth ease-out
fn ease_out(t: f32) -> f32 {
    1.0 - (1.0 - t).powi(3)
}
