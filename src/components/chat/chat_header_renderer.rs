//! Chat Header Renderer Entity - Extracted from app.rs monolith
//!
//! This entity handles unified header rendering for chat splits:
//! - Channel name and stream status
//! - Stream title (when live)
//! - Metadata: game, uptime, viewer count
//! - Settings dropdown and close button
//! - Drag-to-split gesture support
//!
//! Extracted from app.rs to reduce its size from 4,856 lines.
#![allow(clippy::too_many_arguments)]

use super::chat_settings_bar::{active_room_state_label_count, render_chat_settings_tags_inline};
use super::chat_ui::{
    HEADER_STATUS_DOT_SIZE_PX, HeaderMetadataPlan, estimate_header_text_width,
    header_channel_label, render_header_metadata, render_header_metadata_compact,
    render_header_metadata_planned, render_header_stream_status, render_header_title,
};
use crate::backend::irc::room_state::RoomState;
use crate::backend::{self, StreamInfo};
use crate::components::overlays::{
    clear_global_tooltip, clear_global_tooltip_if_source_in_window, show_stream_header_tooltip,
    show_text_tooltip,
};
use crate::components::ui::ButtonExt;
use crate::config;
use crate::managers::{OverlayManager, SplitLayoutManager};
use crate::theme::colors;
use crate::theme::tokens::{FontSize, Radii, RadiiPixels, Spacing};
use gpui::prelude::FluentBuilder;
use gpui::*;

/// Layouts used by the *native channel popup*, which stays independently
/// adaptive: it has no composer rail of its own, so it keeps the inline
/// room-state tags and gains an extra row when they would crowd the context.
///
/// Main split headers don't use this enum. They are a fixed 28px or 40px
/// row whose contents follow [`HeaderWidthBand`], and their ROOMSTATE lives in
/// the composer's `RoomStateIndicator` rail instead.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChannelHeaderLayout {
    Standard,
    ExpandedNarrow,
}

impl ChannelHeaderLayout {
    pub const STANDARD_HEIGHT: f32 = 48.0;
    pub const EXPANDED_NARROW_HEIGHT: f32 = 68.0;
    pub const ONE_LINE_HEIGHT: f32 = 28.0;

    pub fn for_popup_width(width: f32, room_state: Option<&RoomState>) -> Self {
        if width < 620.0 && active_room_state_label_count(room_state) > 0 {
            Self::ExpandedNarrow
        } else {
            Self::Standard
        }
    }

    pub fn height(self) -> f32 {
        match self {
            Self::Standard => Self::STANDARD_HEIGHT,
            Self::ExpandedNarrow => Self::EXPANDED_NARROW_HEIGHT,
        }
    }

    pub fn is_expanded(self) -> bool {
        self == Self::ExpandedNarrow
    }
}

/// Split-width bands for the main header.
///
/// The header's contents depend only on how wide *this* split is, never on how
/// many chat modes the channel happens to have active. Stream information is
/// never dropped by band: while live, every band from `Tiny` up shows the title,
/// category, live duration, and viewer count. The title takes only the space
/// the short metadata leaves and truncates first; the category truncates next.
///
/// | Band | Width | Persistent content |
/// |---|---|---|
/// | `Wide` | `>=720` | dot, channel, title, LIVE/OFFLINE chip, category, uptime, viewers, pin, chatters, add split, menu, close |
/// | `Medium` | `480-719` | as `Wide` without chatters |
/// | `Narrow` | `320-479` | as `Medium` with compact metadata labels, without add split (and without the chip in one-line mode) |
/// | `Tiny` | `240-319` | as `Narrow` |
/// | `Micro` | `<240` | channel, compact uptime and viewers, pin, menu (close moves into the menu) |
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum HeaderWidthBand {
    Micro,
    Tiny,
    Narrow,
    Medium,
    Wide,
}

impl HeaderWidthBand {
    pub fn for_width(width: f32) -> Self {
        if width >= 720.0 {
            Self::Wide
        } else if width >= 480.0 {
            Self::Medium
        } else if width >= 320.0 {
            Self::Narrow
        } else if width >= 240.0 {
            Self::Tiny
        } else {
            Self::Micro
        }
    }

    /// The narrowest band spends its width on the channel name.
    pub fn shows_status_dot(self) -> bool {
        self >= Self::Tiny
    }

    /// The title always renders from `Tiny` up; it is the flexible item that
    /// truncates, never the metadata beside it.
    pub fn shows_title(self) -> bool {
        self >= Self::Tiny
    }

    /// The LIVE/OFFLINE chip duplicates the status dot, so it is the first
    /// thing a crowded row gives up.
    pub fn shows_status_chip(self, one_line: bool) -> bool {
        if one_line {
            self >= Self::Medium
        } else {
            self >= Self::Tiny
        }
    }

    /// Every band keeps the live statistics; only `Micro` drops the category.
    pub fn metadata_plan(self) -> HeaderMetadataPlan {
        if self == Self::Micro {
            HeaderMetadataPlan::LIVE_STATS
        } else {
            HeaderMetadataPlan::ALL
        }
    }

    /// `Live for 2h5m` / `1.2K viewers` from `Medium` up; `2h5m` / `1.2K`
    /// below it and in one-line headers.
    pub fn compact_metadata(self, one_line: bool) -> bool {
        one_line || self <= Self::Narrow
    }

    /// The first action to move into the ellipsis menu.
    pub fn shows_chatters_button(self) -> bool {
        self == Self::Wide
    }

    /// Add split stays visible while a medium-width header still has room for
    /// the channel context and its persistent actions.
    pub fn shows_add_split_button(self) -> bool {
        self >= Self::Medium
    }

    /// Below 240px the close action lives in the menu instead.
    pub fn shows_close_button(self) -> bool {
        self >= Self::Tiny
    }

    /// Fraction of the header the channel name may occupy.
    fn channel_width_fraction(self) -> f32 {
        match self {
            Self::Wide | Self::Medium => 0.32,
            _ => 0.42,
        }
    }
}

pub fn main_split_header_height(compact_one_line: bool) -> f32 {
    if compact_one_line {
        ChannelHeaderLayout::ONE_LINE_HEIGHT
    } else {
        config::MAIN_SPLIT_HEADER_STANDARD_HEIGHT_PX
    }
}

/// Attach the shared rich channel tooltip to a header context region.
///
/// The tooltip covers the channel/title/status region only - action buttons are
/// siblings with their own text tooltips - and it always carries the complete
/// information, including the untruncated title and category.
/// Streamer-mode suppression of viewer count and duration is preserved by the
/// tooltip renderer itself.
fn with_header_stream_tooltip(
    element: Stateful<Div>,
    tooltip_source: String,
    channel: &str,
    stream_info: Option<&StreamInfo>,
    is_placeholder: bool,
    hide_viewer_stats: bool,
) -> Stateful<Div> {
    if is_placeholder {
        return element;
    }
    let channel = channel.to_string();
    let stream_info = stream_info.cloned();
    element.on_hover(move |hovered, window, _app| {
        if *hovered {
            show_stream_header_tooltip(
                window.window_handle(),
                tooltip_source.clone(),
                channel.clone(),
                stream_info.clone(),
                hide_viewer_stats,
                window.mouse_position(),
            );
        } else {
            clear_global_tooltip_if_source_in_window(window.window_handle(), &tooltip_source);
        }
    })
}

/// Channel-context surface for the *native channel popup*.
///
/// Narrow popups get one extra row rather than clipping metadata into
/// room-state labels. The popup has no composer rail, so it is the one surface
/// that still paints ROOMSTATE inline.
pub fn render_channel_header_context(
    channel: &str,
    available_width: f32,
    stream_info: Option<&StreamInfo>,
    room_state: Option<&RoomState>,
    is_placeholder: bool,
    layout: ChannelHeaderLayout,
    tooltip_source: String,
    tooltip_scope: u64,
) -> AnyElement {
    let is_live = stream_info.is_some_and(|info| info.is_live);
    let viewer_count = stream_info.map_or(0, |info| info.viewer_count);
    let game_name = stream_info.and_then(|info| info.game_name.clone());
    let stream_title = stream_info.map(|info| info.title.clone());
    let started_at = stream_info.map(|info| info.started_at);
    let hide_viewer_stats = backend::streamer_mode::should_hide_viewer_count();
    let channel_label_max_width = match layout {
        ChannelHeaderLayout::Standard => (available_width * 0.32).clamp(120.0, 220.0),
        ChannelHeaderLayout::ExpandedNarrow => (available_width * 0.42).clamp(96.0, 160.0),
    };

    let context = div()
        .id(tooltip_source.clone())
        .flex()
        .justify_center()
        .flex_col()
        .gap(px(if layout.is_expanded() {
            Spacing::XXS
        } else {
            Spacing::XS
        }))
        .flex_1()
        .min_w_0()
        .overflow_hidden();
    let context = with_header_stream_tooltip(
        context,
        tooltip_source,
        channel,
        stream_info,
        is_placeholder,
        hide_viewer_stats,
    )
    .child(
        div()
            .flex_1()
            .min_w_0()
            .overflow_hidden()
            .child(render_header_title(
                channel,
                is_live,
                stream_title,
                is_placeholder,
                channel_label_max_width,
                true,
                colors::text_primary(),
            )),
    );

    match layout {
        ChannelHeaderLayout::ExpandedNarrow => context
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(Spacing::XS_PLUS))
                    .min_w_0()
                    .overflow_hidden()
                    .when_some(
                        render_header_stream_status(stream_info, is_placeholder),
                        |element, status| element.child(status),
                    )
                    .child(render_header_metadata_compact(
                        game_name,
                        started_at,
                        viewer_count,
                        hide_viewer_stats,
                        is_live,
                    )),
            )
            .child(render_chat_settings_tags_inline(room_state, tooltip_scope))
            .into_any_element(),
        ChannelHeaderLayout::Standard => context
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(Spacing::XS_PLUS))
                    .min_w_0()
                    .overflow_hidden()
                    .when_some(
                        render_header_stream_status(stream_info, is_placeholder),
                        |element, status| element.child(status),
                    )
                    .child(render_header_metadata(
                        game_name,
                        started_at,
                        viewer_count,
                        hide_viewer_stats,
                        is_live,
                    ))
                    .child(render_chat_settings_tags_inline(room_state, tooltip_scope)),
            )
            .into_any_element(),
    }
}

/// Channel-context surface for a main split header.
///
/// `info_width` is the width the context row actually receives (the split width
/// minus header padding and the action cluster), while `band` was resolved from
/// the split width itself. The standard header is two rows (channel and title,
/// then status chip and metadata); the one-line header puts them side by side
/// with the title taking only the width the metadata leaves. Contents never
/// depend on room state - active chat modes live in the composer rail.
pub fn render_split_header_context(
    channel: &str,
    band: HeaderWidthBand,
    info_width: f32,
    one_line: bool,
    stream_info: Option<&StreamInfo>,
    is_placeholder: bool,
    is_active_split: bool,
    tooltip_source: String,
) -> AnyElement {
    let is_whispers = backend::whispers::is_whispers_channel(channel);
    let is_live = stream_info.is_some_and(|info| info.is_live);
    let viewer_count = stream_info.map_or(0, |info| info.viewer_count);
    let game_name = stream_info.and_then(|info| info.game_name.clone());
    let started_at = stream_info.map(|info| info.started_at);
    let hide_viewer_stats = backend::streamer_mode::should_hide_viewer_count();
    let shows_status_dot = band.shows_status_dot() && !is_whispers;

    let channel_label = header_channel_label(channel, is_placeholder);
    let channel_label_max_width = (info_width * band.channel_width_fraction()).clamp(72.0, 220.0);
    let channel_min_width = estimate_header_text_width(&channel_label, FontSize::MD)
        .min(channel_label_max_width)
        + if shows_status_dot {
            HEADER_STATUS_DOT_SIZE_PX + Spacing::SM
        } else {
            0.0
        };

    let stream_title = stream_info
        .map(|info| info.title.clone())
        .filter(|_| band.shows_title() && !is_placeholder);
    let show_metadata = !is_placeholder && !is_whispers && stream_info.is_some();
    let metadata = show_metadata.then(|| {
        render_header_metadata_planned(
            game_name,
            started_at,
            viewer_count,
            hide_viewer_stats,
            is_live,
            band.compact_metadata(one_line),
            band.metadata_plan(),
        )
    });
    let status_chip = (band.shows_status_chip(one_line) && !is_whispers)
        .then(|| render_header_stream_status(stream_info, is_placeholder))
        .flatten();

    let context = div()
        .id(tooltip_source.clone())
        .flex()
        .justify_center()
        .when(one_line, |element| {
            element.flex_row().items_center().gap(px(Spacing::XS_PLUS))
        })
        .when(!one_line, |element| {
            element.flex_col().gap(px(Spacing::HAIRLINE))
        })
        .flex_1()
        .min_w_0()
        .overflow_hidden();
    let context = if is_whispers {
        context
    } else {
        with_header_stream_tooltip(
            context,
            tooltip_source.clone(),
            channel,
            stream_info,
            is_placeholder,
            hide_viewer_stats,
        )
    }
    .child(
        div()
            .flex_1()
            .min_w_0()
            .when(one_line, |title| title.min_w(px(channel_min_width)))
            .overflow_hidden()
            .child(render_header_title(
                channel,
                is_live,
                stream_title,
                is_placeholder,
                channel_label_max_width,
                shows_status_dot,
                if is_active_split {
                    colors::active_split_label(colors::active_split_header_bg())
                } else {
                    colors::text_primary()
                },
            )),
    );

    if one_line {
        return context
            .when_some(status_chip, |element, status| element.child(status))
            .when_some(metadata, |element, metadata| element.child(metadata))
            .into_any_element();
    }

    let has_second_row = status_chip.is_some() || metadata.is_some();
    context
        .when(has_second_row, |element| {
            element.child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(Spacing::XS_PLUS))
                    .min_w_0()
                    .overflow_hidden()
                    .when_some(status_chip, |row, status| row.child(status))
                    .when_some(metadata, |row, metadata| row.child(metadata)),
            )
        })
        .into_any_element()
}

const MOD_MODE_CHIP_WIDTH_PX: f32 = 36.0;
const MOD_MODE_CHIP_HEIGHT_PX: f32 = crate::components::chat::chat_ui::HEADER_STATUS_CHIP_HEIGHT_PX;

/// Marks a split whose inline moderation buttons are on.
fn render_mod_mode_chip() -> AnyElement {
    div()
        .h(px(MOD_MODE_CHIP_HEIGHT_PX))
        .w(px(MOD_MODE_CHIP_WIDTH_PX - 4.0))
        .flex()
        .flex_shrink_0()
        .items_center()
        .justify_center()
        .rounded(Radii::sm())
        .bg(colors::success().opacity(0.12))
        .border_1()
        .border_color(colors::success().opacity(0.32))
        .text_size(px(FontSize::XS))
        .line_height(px(MOD_MODE_CHIP_HEIGHT_PX - 2.0))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(colors::success())
        .child("MOD")
        .into_any_element()
}

/// The channel's current Twitch pin, as the split header's pin toggle sees it.
///
/// `dismissed` mirrors the split-local dismissal the floating pinned bubble
/// honors, so the header button re-opens a dismissed bubble and hides an open
/// one through the same `SplitPane` state.
pub struct PinnedHeaderToggle {
    pub split_pane: Entity<crate::state::SplitPane>,
    pub message: std::sync::Arc<crate::backend::pinned_chat::PinnedChatMessage>,
    pub dismissed: bool,
}

impl PinnedHeaderToggle {
    fn tooltip(&self) -> String {
        const PREVIEW_CHARS: usize = 120;
        let action = if self.dismissed {
            "Show pinned message"
        } else {
            "Hide pinned message"
        };
        if crate::backend::streamer_mode::is_streamer_mode_active() {
            return action.to_string();
        }
        let message = &self.message;
        let mut preview: String = message.text.chars().take(PREVIEW_CHARS).collect();
        if message.text.chars().count() > PREVIEW_CHARS {
            preview.push('\u{2026}');
        }
        match (message.sender_name.is_empty(), preview.is_empty()) {
            (true, true) => action.to_string(),
            (true, false) => format!("{action} - {preview}"),
            (false, true) => format!("{action} - {}", message.sender_name),
            (false, false) => format!("{action} - {}: {preview}", message.sender_name),
        }
    }
}

/// Entity for rendering chat headers with stream info caching
pub struct ChatHeaderRenderer {}

impl ChatHeaderRenderer {
    pub fn new(_cx: &mut Context<Self>) -> Self {
        Self {}
    }

    /// PERF: Render with pre-fetched stream info (no blocking API calls)
    pub fn render_fast(
        &mut self,
        channel: &str,
        path: &[usize],
        available_width: f32,
        stream_info: Option<&crate::backend::StreamInfo>,
        is_placeholder: bool,
        drag_mode_active: bool,
        split_settings_open: bool,
        compact_one_line: bool,
        is_active_split: bool,
        layout_locked: bool,
        mod_mode: bool,
        pinned: Option<PinnedHeaderToggle>,
        app: Entity<crate::app::EchoApp>,
        overlay_manager: Entity<OverlayManager>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        self.render_unified_header(
            channel,
            path,
            available_width,
            stream_info,
            is_placeholder,
            drag_mode_active,
            split_settings_open,
            compact_one_line,
            is_active_split,
            layout_locked,
            mod_mode,
            pinned,
            app,
            overlay_manager,
            cx,
        )
    }

    /// Render the unified header for a chat split
    #[allow(clippy::too_many_arguments)]
    fn render_unified_header(
        &mut self,
        channel: &str,
        path: &[usize],
        available_width: f32,
        stream_info: Option<&StreamInfo>,
        is_placeholder: bool,
        drag_mode_active: bool,
        split_settings_open: bool,
        compact_one_line: bool,
        is_active_split: bool,
        layout_locked: bool,
        mod_mode: bool,
        pinned: Option<PinnedHeaderToggle>,
        app: Entity<crate::app::EchoApp>,
        overlay_manager: Entity<OverlayManager>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let path_key = SplitLayoutManager::path_key(path);
        let path_key_for_chatters = path_key.clone();
        let path_key_for_add_split = path_key.clone();
        let path_key_for_settings = path_key.clone();
        let path_key_for_hide = path_key.clone();
        let path_key_for_context_menu = path_key.clone();
        let shared_channel: std::sync::Arc<str> = channel.into();
        let channel_for_chatters = std::sync::Arc::clone(&shared_channel);
        let channel_for_settings = std::sync::Arc::clone(&shared_channel);
        let channel_for_context_menu = std::sync::Arc::clone(&shared_channel);
        let chatters_tooltip_source = format!("header-chatters-{}", path_key.as_u64());
        let add_split_tooltip_source = format!("header-add-split-{}", path_key.as_u64());
        let settings_tooltip_source = format!("header-actions-{}", path_key.as_u64());
        let hide_tooltip_source = format!("header-hide-{}", path_key.as_u64());
        let close_tooltip_source = format!("header-close-{}", path_key.as_u64());
        let shared_path: std::sync::Arc<[usize]> = path.into();
        let path_for_drag = std::sync::Arc::clone(&shared_path);
        let path_for_add_split = std::sync::Arc::clone(&shared_path);
        let path_for_close = std::sync::Arc::clone(&shared_path);
        let path_for_hide = std::sync::Arc::clone(&shared_path);
        let path_for_context_menu = shared_path;
        let channel_for_drag = shared_channel;

        let app_for_drag = app.clone();
        let app_for_add_split = app.clone();
        let app_for_close = app.clone();
        let app_for_hide = app.clone();
        let app_for_context_menu = app.clone();
        let overlay_manager_for_context_menu = overlay_manager.clone();

        let header_tooltip_source = format!("header-stream-info-{}", path_key.as_u64());
        let band = HeaderWidthBand::for_width(available_width);
        let show_chatters_button = band.shows_chatters_button()
            && !is_placeholder
            && !backend::whispers::is_whispers_channel(channel);
        let show_add_split_button =
            band.shows_add_split_button() && !is_placeholder && !layout_locked;
        let show_close_button = band.shows_close_button() && !layout_locked;
        let pinned =
            pinned.filter(|_| !is_placeholder && !backend::whispers::is_whispers_channel(channel));
        let pin_tooltip_source = format!("header-pin-{}", path_key.as_u64());
        let header_height = main_split_header_height(compact_one_line);
        let header_padding_x = if compact_one_line { 4.0 } else { 8.0 };
        let action_button_size = if compact_one_line { 24.0 } else { 28.0 };
        let action_count = 2
            + usize::from(pinned.is_some())
            + usize::from(show_chatters_button)
            + usize::from(show_add_split_button)
            + usize::from(show_close_button);
        let actions_width =
            action_count as f32 * action_button_size + action_count.saturating_sub(1) as f32 * 4.0;
        let show_mod_chip = mod_mode && !is_placeholder;
        let mod_chip_width = if show_mod_chip {
            MOD_MODE_CHIP_WIDTH_PX
        } else {
            0.0
        };
        let info_width = (available_width
            - header_padding_x * 2.0
            - header_padding_x
            - actions_width
            - mod_chip_width)
            .max(0.0);
        let feedback_path_key = path_key.as_u64();
        let feedback_header_enabled = crate::benchmark_ui_fixture::feedback_ui_enabled();

        div()
            .on_children_prepainted(move |child_bounds, _window, _cx| {
                if !feedback_header_enabled {
                    return;
                }
                let overlap =
                    child_bounds
                        .first()
                        .zip(child_bounds.get(1))
                        .is_none_or(|(info, actions)| {
                            let info_right: f32 = (info.origin.x + info.size.width).into();
                            let actions_left: f32 = actions.origin.x.into();
                            info_right > actions_left + 0.5
                        });
                crate::benchmark_ui_fixture::publish_feedback_header(feedback_path_key, overlap);
            })
            .id(ElementId::Name(
                format!("unified-header-{}", path_key).into(),
            ))
            .relative()
            .h(px(header_height))
            .flex_shrink_0()
            .w_full()
            .px(px(if compact_one_line {
                Spacing::XS
            } else {
                Spacing::SM
            }))
            .flex()
            .items_center()
            .gap(px(if compact_one_line {
                Spacing::XS
            } else {
                Spacing::SM
            }))
            .bg(if is_active_split {
                colors::active_split_header_bg()
            } else {
                colors::bg_surface()
            })
            .border_b_1()
            .border_color(colors::border_subtle())
            .when(drag_mode_active && !is_placeholder, |div| {
                div.cursor(CursorStyle::PointingHand)
            })
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |_this, event: &MouseDownEvent, window, cx| {
                    if is_placeholder {
                        return;
                    }
                    let path = path_for_drag.clone();
                    let channel = channel_for_drag.clone();
                    let ctrl = window.modifiers().control;
                    app_for_drag.update(cx, |app, app_cx| {
                        if event.click_count >= 2 {
                            app.split_layout
                                .update(app_cx, |layout, _| layout.clear_pending_pane_drag());
                            app.set_active_split(path.to_vec(), app_cx);
                            app.start_change_active_split_channel(app_cx);
                            return;
                        }
                        if ctrl {
                            app.start_pane_drag(path.to_vec(), channel.to_string(), true, app_cx);
                        } else {
                            app.arm_pane_drag(
                                path.to_vec(),
                                channel.to_string(),
                                event.position,
                                app_cx,
                            );
                        }
                    });
                }),
            )
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |_this, _event: &MouseDownEvent, _window, cx| {
                    if is_placeholder {
                        return;
                    }
                    clear_global_tooltip();
                    app_for_context_menu.update(cx, |app, app_cx| {
                        app.set_active_split(path_for_context_menu.to_vec(), app_cx);
                    });
                    overlay_manager_for_context_menu.update(cx, |overlays, _| {
                        overlays.toggle_split_settings(
                            path_key_for_context_menu.clone(),
                            channel_for_context_menu.to_string(),
                        );
                    });
                    cx.notify();
                    cx.stop_propagation();
                }),
            )
            .child(
                div()
                    .id(("header-info-row", path_key.as_u64()))
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .gap(px(Spacing::XS))
                    .when(show_mod_chip, |row| row.child(render_mod_mode_chip()))
                    .child(div().flex_1().min_w_0().child(render_split_header_context(
                        channel,
                        band,
                        info_width,
                        compact_one_line,
                        stream_info,
                        is_placeholder,
                        is_active_split,
                        header_tooltip_source,
                    ))),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(Spacing::XS))
                    .flex_shrink_0()
                    .when_some(pinned, |actions, pinned| {
                        let tooltip = pinned.tooltip();
                        let bubble_open = !pinned.dismissed;
                        let app = app.clone();
                        actions.child(
                            small_icon_button_style(compact_one_line)
                                .id(("pinned-chat-btn", path_key.as_u64()))
                                .role(Role::Button)
                                .aria_label(if bubble_open {
                                    "Hide pinned message"
                                } else {
                                    "Show pinned message"
                                })
                                .when(bubble_open, |button| {
                                    button
                                        .bg(colors::accent_transparent(0.12))
                                        .border_color(colors::accent_transparent(0.42))
                                })
                                .hover(|style| {
                                    style.bg(colors::bg_hover()).border_color(colors::border())
                                })
                                .on_activate(cx.listener(move |_this, _, _, cx| {
                                    clear_global_tooltip();
                                    let message = &pinned.message;
                                    pinned.split_pane.update(cx, |pane, pane_cx| {
                                        if pinned.dismissed {
                                            pane.clear_dismissed_pinned_chat(pane_cx);
                                        } else {
                                            pane.dismiss_pinned_chat(
                                                &message.message_id,
                                                message.updated_at.as_deref(),
                                                &message.text,
                                                pane_cx,
                                            );
                                        }
                                    });
                                    app.update(cx, |_app, app_cx| app_cx.notify());
                                    cx.stop_propagation();
                                }))
                                .on_hover({
                                    let source = pin_tooltip_source.clone();
                                    move |hovered, window, _app| {
                                        if *hovered {
                                            show_text_tooltip(
                                                window.window_handle(),
                                                source.clone(),
                                                tooltip.clone(),
                                                window.mouse_position(),
                                            );
                                        } else {
                                            clear_global_tooltip_if_source_in_window(
                                                window.window_handle(),
                                                &source,
                                            );
                                        }
                                    }
                                })
                                .child(svg().path("icons/pin.svg").size(px(13.0)).text_color(
                                    if bubble_open {
                                        colors::accent()
                                    } else {
                                        colors::text_secondary()
                                    },
                                )),
                        )
                    })
                    .when(show_chatters_button, |actions| {
                        actions.child(
                            small_icon_button_style(compact_one_line)
                                .id(("chatters-btn", path_key_for_chatters.as_u64()))
                                .role(Role::Button)
                                .aria_label("Open chatters")
                                .hover(|style| {
                                    style.bg(colors::bg_hover()).border_color(colors::border())
                                })
                                .on_accessible_activate({
                                    let channel = channel_for_chatters.clone();
                                    cx.listener(move |_this, _, _, cx| {
                                        clear_global_tooltip();
                                        backend::open_chatter_list_async(channel.to_string());
                                        cx.notify();
                                        cx.stop_propagation();
                                    })
                                })
                                .on_hover({
                                    let source = chatters_tooltip_source.clone();
                                    move |hovered, window, _app| {
                                        if *hovered {
                                            show_text_tooltip(
                                                window.window_handle(),
                                                source.clone(),
                                                "Open chatters".to_string(),
                                                window.mouse_position(),
                                            );
                                        } else {
                                            clear_global_tooltip_if_source_in_window(
                                                window.window_handle(),
                                                &source,
                                            );
                                        }
                                    }
                                })
                                .child(
                                    svg()
                                        .path("icons/user.svg")
                                        .size(px(13.0))
                                        .text_color(colors::text_secondary()),
                                ),
                        )
                    })
                    .when(show_add_split_button, |actions| {
                        actions.child(
                            small_icon_button_style(compact_one_line)
                                .id(("add-split-btn", path_key_for_add_split.as_u64()))
                                .role(Role::Button)
                                .aria_label("Add split to right")
                                .hover(|style| {
                                    style.bg(colors::bg_hover()).border_color(colors::border())
                                })
                                .on_accessible_activate({
                                    let path = path_for_add_split.clone();
                                    let app = app_for_add_split.clone();
                                    cx.listener(move |_this, _, _, cx| {
                                        clear_global_tooltip();
                                        app.update(cx, |app, app_cx| {
                                            app.add_empty_split(
                                                path.to_vec(),
                                                crate::state::AddSplitDirection::Right,
                                                app_cx,
                                            );
                                        });
                                        cx.stop_propagation();
                                    })
                                })
                                .on_hover({
                                    let source = add_split_tooltip_source.clone();
                                    move |hovered, window, _app| {
                                        if *hovered {
                                            show_text_tooltip(
                                                window.window_handle(),
                                                source.clone(),
                                                "Add split to right".to_string(),
                                                window.mouse_position(),
                                            );
                                        } else {
                                            clear_global_tooltip_if_source_in_window(
                                                window.window_handle(),
                                                &source,
                                            );
                                        }
                                    }
                                })
                                .child(
                                    div()
                                        .text_xs()
                                        .font_weight(FontWeight::BOLD)
                                        .text_color(colors::text_secondary())
                                        .child("+"),
                                ),
                        )
                    })
                    .child(
                        small_icon_button_style(compact_one_line)
                            .id(("hide-header-btn", path_key_for_hide.as_u64()))
                            .role(Role::Button)
                            .aria_label("Hide header")
                            .hover(|style| {
                                style.bg(colors::bg_hover()).border_color(colors::border())
                            })
                            .on_accessible_activate({
                                let path = path_for_hide.clone();
                                cx.listener(move |_this, _, _, cx| {
                                    clear_global_tooltip();
                                    app_for_hide.update(cx, |app, app_cx| {
                                        app.set_split_header_collapsed(path.to_vec(), true, app_cx);
                                    });
                                    cx.stop_propagation();
                                })
                            })
                            .on_hover({
                                let source = hide_tooltip_source.clone();
                                move |hovered, window, _app| {
                                    if *hovered {
                                        show_text_tooltip(
                                            window.window_handle(),
                                            source.clone(),
                                            "Hide header".to_string(),
                                            window.mouse_position(),
                                        );
                                    } else {
                                        clear_global_tooltip_if_source_in_window(
                                            window.window_handle(),
                                            &source,
                                        );
                                    }
                                }
                            })
                            .child(div().text_size(px(FontSize::XS)).child("−")),
                    )
                    .child(
                        small_icon_button_style(compact_one_line)
                            .id(("settings-btn", path_key_for_settings.as_u64()))
                            .role(Role::Button)
                            .aria_label("Split actions")
                            .when(split_settings_open, |button| {
                                button
                                    .bg(colors::accent_transparent(0.12))
                                    .border_color(colors::accent_transparent(0.42))
                            })
                            .hover(|style| {
                                style.bg(colors::bg_hover()).border_color(colors::border())
                            })
                            .on_accessible_activate({
                                let pk = path_key_for_settings;
                                let ch = channel_for_settings.clone();
                                cx.listener(move |_this, _, _, cx| {
                                    clear_global_tooltip();
                                    overlay_manager.update(cx, |o, _| {
                                        o.toggle_split_settings(pk.clone(), ch.to_string());
                                    });
                                    cx.notify();
                                    cx.stop_propagation();
                                })
                            })
                            .on_hover({
                                let source = settings_tooltip_source.clone();
                                move |hovered, window, _app| {
                                    if *hovered {
                                        show_text_tooltip(
                                            window.window_handle(),
                                            source.clone(),
                                            "Split actions".to_string(),
                                            window.mouse_position(),
                                        );
                                    } else {
                                        clear_global_tooltip_if_source_in_window(
                                            window.window_handle(),
                                            &source,
                                        );
                                    }
                                }
                            })
                            .child(
                                div()
                                    .text_xs()
                                    .font_weight(FontWeight::BOLD)
                                    .text_color(colors::text_secondary())
                                    .child("⋯"),
                            ),
                    )
                    .when(show_close_button, |actions| {
                        actions.child(
                            small_icon_button_style(compact_one_line)
                                .id(("close-split-btn", path_key.as_u64()))
                                .role(Role::Button)
                                .aria_label("Close split")
                                .hover(|style| {
                                    style
                                        .bg(colors::bg_hover())
                                        .border_color(colors::signal_red())
                                        .text_color(colors::signal_red())
                                })
                                .on_activate({
                                    let path = path_for_close.clone();
                                    cx.listener(move |_this, _, _, cx| {
                                        clear_global_tooltip();
                                        app_for_close.update(cx, |app, app_cx| {
                                            app.close_pane(path.to_vec(), app_cx);
                                        });
                                    })
                                })
                                .on_hover({
                                    let source = close_tooltip_source.clone();
                                    move |hovered, window, _app| {
                                        if *hovered {
                                            show_text_tooltip(
                                                window.window_handle(),
                                                source.clone(),
                                                "Close split".to_string(),
                                                window.mouse_position(),
                                            );
                                        } else {
                                            clear_global_tooltip_if_source_in_window(
                                                window.window_handle(),
                                                &source,
                                            );
                                        }
                                    }
                                })
                                .child(div().text_xs().child(crate::icons::CLOSE_GLYPH)),
                        )
                    }),
            )
    }
}

/// Small icon button style for header actions
fn small_icon_button_style(compact: bool) -> Div {
    div()
        .w(px(if compact { 24.0 } else { 28.0 }))
        .h(px(if compact { 24.0 } else { 28.0 }))
        .flex()
        .items_center()
        .justify_center()
        .rounded(Radii::md())
        .bg(gpui::transparent_black())
        .border_1()
        .border_color(gpui::transparent_black())
        .text_color(colors::text_secondary())
        .cursor_pointer()
}
