//! Chat UI helper functions
//!
//! Pure rendering functions extracted from app.rs to reduce code duplication
//! and improve maintainability. These functions take data and return elements
//! without needing access to EchoApp state or event handlers.

use crate::backend::StreamInfo;
use crate::theme::colors;
use crate::theme::tokens::{FontSize, Radii, RadiiPixels, Spacing};
use gpui::prelude::FluentBuilder;
use gpui::*;

/// Rough shaped width of a header string.
///
/// The one-line header reserves the channel label's width *before* laying
/// anything out, so it cannot ask the text system. 0.55em is a deliberately
/// conservative average for the UI font at these sizes: overestimating only
/// costs the title a few pixels, while underestimating would clip the channel.
pub fn estimate_header_text_width(text: &str, font_px: f32) -> f32 {
    text.chars().count() as f32 * font_px * 0.55
}

/// The channel name as shown in titles (no `#`), or the placeholder affordance.
pub fn header_channel_label(channel: &str, is_placeholder: bool) -> String {
    if is_placeholder {
        "Add channel".to_string()
    } else if crate::backend::whispers::is_whispers_channel(channel) {
        "Whispers".to_string()
    } else {
        channel.trim_start_matches('#').to_string()
    }
}

/// Diameter of the live/offline dot ahead of the channel label.
pub const HEADER_STATUS_DOT_SIZE_PX: f32 = 7.0;

/// Line box of the header title row.
///
/// The default line height for the 14px label is about 22.6px, which pushed the
/// two-row header (title row + 1px gap + status chip) past its fixed 40px and
/// clipped the chip's lower edge. 20px keeps the label's glyphs whole and leaves
/// the stacked rows 37px tall.
const HEADER_TITLE_LINE_HEIGHT_PX: f32 = 20.0;

/// Render the title section of the unified header
/// Shows: status dot, channel name, separator, stream title
pub fn render_header_title(
    channel: &str,
    is_live: bool,
    stream_title: Option<String>,
    is_placeholder: bool,
    channel_label_max_width: f32,
    show_status_dot: bool,
    label_color: Hsla,
) -> impl IntoElement + use<> {
    let title = header_channel_label(channel, is_placeholder);

    let stream_title = stream_title.and_then(|title| {
        let title = title.trim();
        (!title.is_empty()).then(|| title.to_string())
    });

    div()
        .flex()
        .items_center()
        .gap(px(Spacing::SM))
        .flex_1()
        .min_w_0()
        .line_height(px(HEADER_TITLE_LINE_HEIGHT_PX))
        .overflow_hidden()
        .when(show_status_dot, |el| {
            el.child(
                div()
                    .size(px(HEADER_STATUS_DOT_SIZE_PX))
                    .rounded(Radii::full())
                    .bg(if is_live {
                        colors::signal_red()
                    } else {
                        colors::text_muted()
                    })
                    .flex_shrink_0(),
            )
        })
        .child(
            div()
                .text_size(px(FontSize::MD))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(label_color)
                .min_w_0()
                .max_w(px(channel_label_max_width))
                .truncate()
                .whitespace_nowrap()
                .flex_shrink_0()
                .child(title),
        )
        .when_some(stream_title.filter(|_| !is_placeholder), |el, title| {
            el.child(
                div()
                    .text_size(px(FontSize::SM))
                    .text_color(colors::text_muted())
                    .flex_shrink_0()
                    .mx(px(Spacing::XXS))
                    .child("•"),
            )
            .child(
                div()
                    .text_size(px(FontSize::SM))
                    .text_color(colors::text_secondary())
                    .overflow_hidden()
                    .truncate()
                    .whitespace_nowrap()
                    .flex_1()
                    .min_w_0()
                    .child(title),
            )
        })
}

const HEADER_META_GAP_PX: f32 = Spacing::XS_PLUS;

/// Which header metadata items a split header paints.
///
/// Main split headers paint every item at every width except the very
/// narrowest band, which keeps only uptime and viewers; see
/// `HeaderWidthBand::metadata_plan`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HeaderMetadataPlan {
    pub category: bool,
    pub uptime: bool,
    pub viewers: bool,
}

impl HeaderMetadataPlan {
    pub const ALL: Self = Self {
        category: true,
        uptime: true,
        viewers: true,
    };
    pub const LIVE_STATS: Self = Self {
        category: false,
        uptime: true,
        viewers: true,
    };
}

fn header_viewer_label(viewer_count: u32, compact: bool) -> String {
    if compact {
        format_viewer_count(viewer_count)
    } else if viewer_count == 1 {
        format!("{} viewer", format_viewer_count(viewer_count))
    } else {
        format!("{} viewers", format_viewer_count(viewer_count))
    }
}

/// Render the metadata section of the unified header
/// Shows: category, explicit live duration, and explicit viewer count.
pub fn render_header_metadata(
    game_name: Option<String>,
    started_at: Option<i64>,
    viewer_count: u32,
    hide_viewer_stats: bool,
    is_live: bool,
) -> impl IntoElement {
    render_header_metadata_density(
        game_name,
        started_at,
        viewer_count,
        hide_viewer_stats,
        is_live,
        false,
        HeaderMetadataPlan::ALL,
    )
}

/// Render only the metadata a [`HeaderMetadataPlan`] admitted.
pub fn render_header_metadata_planned(
    game_name: Option<String>,
    started_at: Option<i64>,
    viewer_count: u32,
    hide_viewer_stats: bool,
    is_live: bool,
    compact: bool,
    plan: HeaderMetadataPlan,
) -> impl IntoElement {
    render_header_metadata_density(
        game_name,
        started_at,
        viewer_count,
        hide_viewer_stats,
        is_live,
        compact,
        plan,
    )
}

/// Render the same metadata in a narrow-header grammar. Labels are shortened,
/// but category, uptime, and viewer information retain their meaning and the
/// full prose remains available in the shared header tooltip.
pub fn render_header_metadata_compact(
    game_name: Option<String>,
    started_at: Option<i64>,
    viewer_count: u32,
    hide_viewer_stats: bool,
    is_live: bool,
) -> impl IntoElement {
    render_header_metadata_density(
        game_name,
        started_at,
        viewer_count,
        hide_viewer_stats,
        is_live,
        true,
        HeaderMetadataPlan::ALL,
    )
}

fn render_header_metadata_density(
    game_name: Option<String>,
    started_at: Option<i64>,
    viewer_count: u32,
    hide_viewer_stats: bool,
    is_live: bool,
    compact: bool,
    plan: HeaderMetadataPlan,
) -> impl IntoElement {
    let game_name = game_name.filter(|_| plan.category).and_then(|game| {
        let game = game.trim();
        (!game.is_empty()).then(|| game.to_string())
    });
    let has_game = game_name.is_some();
    let uptime = started_at
        .filter(|_| plan.uptime)
        .filter(|&ts| ts > 0 && is_live && !hide_viewer_stats);
    let has_uptime = uptime.is_some();
    let show_viewers = plan.viewers && is_live && !hide_viewer_stats;
    let viewer_label = header_viewer_label(viewer_count, compact);

    div()
        .flex()
        .items_center()
        .gap(px(HEADER_META_GAP_PX))
        .min_w_0()
        .overflow_hidden()
        .when_some(game_name, |el, game| {
            el.child(
                div()
                    .text_size(px(FontSize::XS))
                    .text_color(colors::text_secondary())
                    .truncate()
                    .whitespace_nowrap()
                    .max_w(px(if compact { 132.0 } else { 180.0 }))
                    .min_w_0()
                    .child(game),
            )
        })
        .when_some(uptime, |el, ts| {
            el.when(has_game, |el| {
                el.child(
                    div()
                        .text_size(px(FontSize::XS))
                        .text_color(colors::text_muted())
                        .flex_shrink_0()
                        .child("•"),
                )
            })
            .child(
                div()
                    .text_size(px(FontSize::XS))
                    .text_color(colors::text_secondary())
                    .whitespace_nowrap()
                    .flex_shrink_0()
                    .child(if compact {
                        format_uptime(ts)
                    } else {
                        format!("Live for {}", format_uptime(ts))
                    }),
            )
        })
        .when(show_viewers, |el| {
            el.when(has_game || has_uptime, |el| {
                el.child(
                    div()
                        .text_size(px(FontSize::XS))
                        .text_color(colors::text_muted())
                        .flex_shrink_0()
                        .child("•"),
                )
            })
            .child(
                div()
                    .text_size(px(FontSize::XS))
                    .text_color(colors::signal_red())
                    .whitespace_nowrap()
                    .flex_shrink_0()
                    .child(viewer_label),
            )
        })
}

/// Shared with the MOD chip so both header badges line up.
pub const HEADER_STATUS_CHIP_HEIGHT_PX: f32 = 16.0;
const HEADER_STATUS_CHIP_BORDER_PX: f32 = 1.0;
const HEADER_STATUS_CHIP_PADDING_X_PX: f32 = Spacing::XS_PLUS;

fn header_stream_status_label(is_live: bool) -> &'static str {
    if is_live { "LIVE" } else { "OFFLINE" }
}

/// Render the primary live/offline state as a compact, readable badge.
pub fn render_header_stream_status(
    stream_info: Option<&StreamInfo>,
    is_placeholder: bool,
) -> Option<AnyElement> {
    if is_placeholder {
        return None;
    }

    let is_live = stream_info.is_some_and(|info| info.is_live);
    let label = header_stream_status_label(is_live);
    let (color, background, border) = if is_live {
        (
            colors::signal_red(),
            colors::signal_red().opacity(0.12),
            colors::signal_red().opacity(0.28),
        )
    } else {
        (
            colors::text_secondary(),
            colors::bg_elevated(),
            colors::border_subtle(),
        )
    };

    Some(
        div()
            .h(px(HEADER_STATUS_CHIP_HEIGHT_PX))
            .px(px(HEADER_STATUS_CHIP_PADDING_X_PX))
            .flex()
            .flex_shrink_0()
            .items_center()
            .rounded(Radii::sm())
            .bg(background)
            .border(px(HEADER_STATUS_CHIP_BORDER_PX))
            .border_color(border)
            .text_size(px(FontSize::XS))
            .line_height(px(
                HEADER_STATUS_CHIP_HEIGHT_PX - 2.0 * HEADER_STATUS_CHIP_BORDER_PX
            ))
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(color)
            .child(label)
            .into_any_element(),
    )
}

/// Format uptime from Unix timestamp to "Xh Ym" string
pub fn format_uptime(started_at_unix: i64) -> String {
    let now = crate::utils::unix_now_secs() as i64;
    let diff_secs = now - started_at_unix;
    if diff_secs < 0 {
        return "0m".to_string();
    }
    let hours = diff_secs / 3600;
    let minutes = (diff_secs % 3600) / 60;
    if hours > 0 {
        format!("{}h{}m", hours, minutes)
    } else {
        format!("{}m", minutes)
    }
}

/// Format viewer count with K suffix for thousands
pub fn format_viewer_count(count: u32) -> String {
    if count >= 1000 {
        format!("{:.1}K", count as f32 / 1000.0)
    } else {
        count.to_string()
    }
}
