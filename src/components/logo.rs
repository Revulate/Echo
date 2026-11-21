//! Echo Logo Component
//!
//! Renders the animated Echo logo with connection status colors.
//! Uses SVG asset with currentColor, colored via text_color style.
//!
//! Note: GPUI doesn't support SVG gradients, so we use solid colors.

use std::time::Duration;

use gpui::*;

use crate::backend::ConnectionStatus;
use crate::theme::colors;

/// Most frames a second the window presents for the pulse alone, the old
/// timer's focused rate (one step every 40 ms). The window paces these
/// presents itself; they are not aligned to the emote animation grid.
const PULSE_MAX_FPS: f32 = 25.0;

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
/// - Reconnecting: Orange, pulsing every 2 s down to 40% opacity
/// - Disconnected: Red, pulsing every 1.5 s down to 30% opacity
///
/// The pulse is a timed transformation of the SVG: the window advances its
/// alpha in the presented scene before each present, at most
/// [`PULSE_MAX_FPS`] times a second, without re-rendering the owning view.
/// Under reduced motion the logo holds the middle of the pulse's range
/// instead (70% or 65%), so a non-connected logo is dimmer than a connected
/// one, not only differently colored.
pub fn render_logo(
    connection_status: &ConnectionStatus,
    size: f32,
    reduce_motion: bool,
) -> AnyElement {
    let color = status_color(connection_status);
    let logo = |id: &'static str, svg: Svg| {
        div()
            .id(id)
            .size(px(size))
            .flex()
            .items_center()
            .justify_center()
            .child(svg.size(px(size)).text_color(color))
    };
    let svg = svg().path("icons/echo-logo.svg");

    let (id, period, min_opacity) = match connection_status {
        ConnectionStatus::Connected => {
            return logo("echo-logo", svg)
                .with_animation(
                    "logo-fade-in",
                    Animation::new(Duration::from_millis(600)).with_easing(ease_out),
                    |el, delta| el.opacity(delta),
                )
                .into_any_element();
        }
        ConnectionStatus::Reconnecting => {
            ("echo-logo-reconnecting", Duration::from_millis(2000), 0.4)
        }
        ConnectionStatus::Disconnected => {
            ("echo-logo-disconnected", Duration::from_millis(1500), 0.3)
        }
    };

    // Full opacity down to the minimum and back once per period. A linear
    // ramp each way is the closest of the available easings to the old
    // sine-cubic breath (within 0.054 of its normalized curve).
    if reduce_motion {
        let resting = Transformation::alpha((1.0 + min_opacity) / 2.0);
        return logo(id, svg.with_transformation(resting)).into_any_element();
    }
    let pulse = TimedTransformation::new(period)
        .from(Transformation::alpha(1.0))
        .to(Transformation::alpha(min_opacity))
        .alternate()
        .with_max_fps(PULSE_MAX_FPS);
    logo(id, svg.with_timed_transformation(pulse)).into_any_element()
}

/// Easing function for smooth ease-out
fn ease_out(t: f32) -> f32 {
    1.0 - (1.0 - t).powi(3)
}
