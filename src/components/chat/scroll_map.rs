//! Scroll map (mini-map) component for chat visualization
//!
//! Scroll map that shows:
//! - Message density visualization
//! - Color-coded markers for search results, mentions, subscriptions, first messages, raids
//! - Click-to-jump navigation with viewport indicator
//! - Hover expansion for better visibility

use crate::backend::ChatMessage;
use crate::backend::scroll_markers::ScrollMarkerPriority as MarkerPriority;
use crate::backend::scroll_markers::{ScrollMarkerInput, ScrollMarkerKind, classify_scroll_marker};
use crate::theme::colors;
use crate::theme::tokens::{Radii, RadiiPixels};
use gpui::prelude::FluentBuilder;
use gpui::*;
use rustc_hash::FxHashSet;
use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;

/// Fixed gutter reserved beside the message canvas. Hover changes only the rail inside it.
pub(super) const SCROLL_MAP_GUTTER_WIDTH: f32 = 14.0;
/// Width of the visible scroll-map rail while inactive.
const SCROLL_MAP_RAIL_WIDTH: f32 = 8.0;
/// Marker height in pixels
const MARKER_HEIGHT: f32 = 2.0;
/// Minimum viewport indicator height
const MIN_VIEWPORT_HEIGHT: f32 = 10.0;
/// Horizontal inset so the rail reads more like a minimap gutter than a full solid bar.
const SCROLL_MAP_TRACK_INSET: f32 = 1.0;
/// Vertical breathing room around the marker track.
const SCROLL_MAP_TRACK_VERTICAL_INSET: f32 = 4.0;

type ScrollMapJumpCallback = Arc<dyn Fn(f32, bool, &mut App) + Send + Sync + 'static>;

#[derive(Clone)]
struct ScrollMapDrag {
    rail_id: SharedString,
    /// Distance from the thumb's top edge to the cursor, in track space.
    ///
    /// GPUI hands the drag payload to `on_drag_move` through the same `Arc` it
    /// was constructed with, so this is filled in from the constructor's cursor
    /// offset at drag-start rather than at mouse-down: the rail re-renders
    /// between press and drag threshold in a live chat pane, and a cell shared
    /// only within one frame's closures would always be read back at its
    /// default.
    grab_offset_y: Cell<f32>,
}

impl Render for ScrollMapDrag {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div().size_0()
    }
}

/// Precomputed marker entry for the scroll map.
#[derive(Clone)]
pub struct ScrollMarker {
    pub position: f32,
    pub color: Hsla,
    pub priority: MarkerPriority,
}

/// Geometry shared by the chat viewport and its scroll-map rail.
///
/// Markers, thumb and jumps all use message-index space (row index over row
/// count), so a marker click lands on its message whatever the row heights.
#[derive(Clone, Copy, Debug)]
pub struct ScrollMapViewport {
    pub new_message_boundary_position: Option<f32>,
    /// Viewport top edge in index space.
    pub top_position: f32,
    /// Viewport bottom edge in index space.
    pub bottom_position: f32,
    pub visible_height: f32,
}

/// Resolve a scroll-map mark's color from the message's own highlight color,
/// forced opaque (the exact highlight color with alpha 255).
///
/// Returns `None` for messages with no highlight type (search/moderation/plain
/// content mentions), which fall back to the per-kind palette.
fn highlight_marker_color(highlight_type: Option<&crate::backend::HighlightType>) -> Option<Hsla> {
    use crate::components::chat::canvas_renderer::layout::HighlightColors;
    use crate::components::chat::canvas_renderer::layout::HighlightType as RenderHighlight;
    highlight_type
        .map(RenderHighlight::from)
        .and_then(|h| h.border_color())
}

fn marker_color(kind: ScrollMarkerKind) -> Hsla {
    match kind {
        ScrollMarkerKind::SearchResult
        | ScrollMarkerKind::HighlightedMessage
        | ScrollMarkerKind::ChannelPointRedeem
        | ScrollMarkerKind::BitsBadge => colors::accent(),
        ScrollMarkerKind::Moderation => colors::signal_red(),
        ScrollMarkerKind::Mention => colors::highlight_mention(),
        ScrollMarkerKind::Whisper => colors::highlight_whisper(),
        ScrollMarkerKind::Raid => colors::highlight_raid(),
        ScrollMarkerKind::Subscription => colors::highlight_subscription(),
        ScrollMarkerKind::Announcement => colors::highlight_announcement(),
        ScrollMarkerKind::WatchStreak => colors::highlight_watchstreak(),
        ScrollMarkerKind::FirstMessage | ScrollMarkerKind::ReturningChatter => {
            colors::highlight_first_message()
        }
    }
}

/// Render scroll map as inline element within chat pane
///
/// Uses highlight_type from MessageFlags for proper color coding
pub fn compute_scroll_markers(
    messages: &[Arc<ChatMessage>],
    our_username: Option<&str>,
    search_result_ids: Option<&FxHashSet<String>>,
) -> Vec<ScrollMarker> {
    let total = messages.len();
    if total == 0 {
        return Vec::new();
    }

    let mut markers: Vec<ScrollMarker> = Vec::with_capacity(32);

    for (idx, msg) in messages.iter().enumerate() {
        let position = idx as f32 / total as f32;

        let is_search_result = search_result_ids
            .map(|ids| ids.contains(msg.id.as_ref()))
            .unwrap_or(false);

        if let Some(decision) = classify_scroll_marker(ScrollMarkerInput {
            content: &msg.content,
            is_system: msg.flags.is_system,
            is_first_message: msg.flags.is_first_message,
            highlight_type: msg.flags.highlight_type.as_ref(),
            our_username,
            is_search_result,
        }) {
            let color = highlight_marker_color(msg.flags.highlight_type.as_ref())
                .unwrap_or_else(|| marker_color(decision.kind));
            markers.push(ScrollMarker {
                position,
                color,
                priority: decision.priority,
            });
        }
    }

    markers.sort_by_key(|a| a.priority);
    markers
}

fn marker_top_px(position: f32, track_height: f32) -> f32 {
    (position * track_height).min((track_height - MARKER_HEIGHT).max(0.0))
}

/// Keeps one marker per `MARKER_HEIGHT` track row: the one that would paint
/// last (markers arrive sorted by ascending priority), so the element count is
/// bounded by the track height instead of the marker count. Survivors keep
/// their original paint order.
fn bucket_markers_per_row(
    markers: &[ScrollMarker],
    track_height: f32,
) -> impl Iterator<Item = (f32, &ScrollMarker)> {
    let row_count = (track_height / MARKER_HEIGHT).ceil().max(1.0) as usize;
    let mut winners: Vec<Option<usize>> = vec![None; row_count];
    for (index, marker) in markers.iter().enumerate() {
        let top_px = marker_top_px(marker.position, track_height);
        let row = ((top_px.max(0.0) / MARKER_HEIGHT) as usize).min(row_count - 1);
        winners[row] = Some(index);
    }
    let mut survivors: Vec<usize> = winners.into_iter().flatten().collect();
    survivors.sort_unstable();
    survivors.into_iter().map(move |index| {
        let marker = &markers[index];
        (marker_top_px(marker.position, track_height), marker)
    })
}

/// Render scroll map from precomputed markers.
///
/// `on_jump`: optional callback receiving the index-space viewport top (1.0 means
/// the bottom) plus whether the pointer is actively dragging. Track clicks stay
/// animated; drag updates are immediate.
pub fn render_scroll_map_from_markers(
    markers: &[ScrollMarker],
    messages: &[Arc<ChatMessage>],
    viewport: ScrollMapViewport,
    drag_id: SharedString,
    on_jump: Option<ScrollMapJumpCallback>,
) -> impl IntoElement + use<> {
    let total = messages.len();
    if total == 0 {
        return div()
            .w(px(SCROLL_MAP_GUTTER_WIDTH))
            .h_full()
            .flex_shrink_0()
            .into_any_element();
    }

    let top_position = viewport.top_position.clamp(0.0, 1.0);
    let visible_portion = (viewport.bottom_position - top_position).clamp(0.0, 1.0);
    let free_portion = 1.0 - visible_portion;

    let track_height = (viewport.visible_height - SCROLL_MAP_TRACK_VERTICAL_INSET * 2.0).max(1.0);
    let viewport_height = (visible_portion * track_height)
        .max(MIN_VIEWPORT_HEIGHT)
        .min(track_height);
    let viewport_travel = (track_height - viewport_height).max(0.0);
    let viewport_top = if free_portion > 0.0 {
        (top_position / free_portion).clamp(0.0, 1.0) * viewport_travel
    } else {
        0.0
    };
    let thumb_top_to_position = move |thumb_top: f32| {
        if viewport_travel <= 0.0 || thumb_top >= viewport_travel - 0.5 {
            1.0
        } else {
            thumb_top / viewport_travel * free_portion
        }
    };

    let inner = div()
        .id("scroll-map-inline")
        .w_full()
        .h_full()
        .bg(colors::bg_void().opacity(0.28))
        .relative()
        .children(
            bucket_markers_per_row(markers, track_height).map(move |(top_px, marker)| {
                div()
                    .absolute()
                    .left(px(SCROLL_MAP_TRACK_INSET))
                    .right(px(SCROLL_MAP_TRACK_INSET))
                    .h(px(MARKER_HEIGHT))
                    .top(px(top_px))
                    .bg(marker.color)
                    .rounded(Radii::xs())
            }),
        )
        .when_some(viewport.new_message_boundary_position, |track, position| {
            let top_px = marker_top_px(position, track_height);
            track.child(
                div()
                    .absolute()
                    .left(px(SCROLL_MAP_TRACK_INSET))
                    .right(px(SCROLL_MAP_TRACK_INSET))
                    .h(px(MARKER_HEIGHT + 1.0))
                    .top(px(top_px))
                    .bg(colors::accent())
                    .rounded(Radii::xs()),
            )
        })
        .child(
            div()
                .absolute()
                .left(px(SCROLL_MAP_TRACK_INSET))
                .right(px(SCROLL_MAP_TRACK_INSET))
                .top(px(viewport_top))
                .h(px(viewport_height))
                .rounded(Radii::xs())
                .bg(colors::accent().opacity(0.13))
                .border_1()
                .border_color(colors::accent().opacity(0.46))
                .group_hover("scroll-map-inline", |style| {
                    style
                        .bg(colors::accent().opacity(0.2))
                        .border_color(colors::accent().opacity(0.72))
                }),
        );

    let track_bounds = Rc::new(Cell::new(None::<Bounds<Pixels>>));
    let prepaint_track_bounds = Rc::clone(&track_bounds);

    div()
        .group("scroll-map-inline")
        .w(px(SCROLL_MAP_GUTTER_WIDTH))
        .h_full()
        .py(px(SCROLL_MAP_TRACK_VERTICAL_INSET))
        .flex()
        .justify_end()
        .flex_shrink_0()
        .rounded(Radii::sm())
        .hover(|style| style.bg(colors::hover_surface()))
        .on_children_prepainted(move |bounds, _window, _cx| {
            prepaint_track_bounds.set(bounds.first().copied());
        })
        .id(drag_id.clone())
        .when_some(on_jump, move |el, cb| {
            let click_track_bounds = Rc::clone(&track_bounds);
            let click_cb = Arc::clone(&cb);
            let drag_cb = Arc::clone(&cb);
            let drag_id_for_check = drag_id.clone();
            el.cursor(CursorStyle::OpenHand)
                .on_drag(
                    ScrollMapDrag {
                        rail_id: drag_id,
                        grab_offset_y: Cell::new(viewport_height / 2.0),
                    },
                    move |drag, cursor_offset, _, cx| {
                        let cursor_y: f32 = cursor_offset.y.into();
                        let track_y = cursor_y - SCROLL_MAP_TRACK_VERTICAL_INSET;
                        let inside_thumb =
                            track_y >= viewport_top && track_y <= viewport_top + viewport_height;
                        drag.grab_offset_y.set(if inside_thumb {
                            (track_y - viewport_top).clamp(0.0, viewport_height)
                        } else {
                            viewport_height / 2.0
                        });
                        cx.stop_propagation();
                        cx.new(|_| drag.clone())
                    },
                )
                .on_drag_move(
                    move |event: &DragMoveEvent<ScrollMapDrag>, _window, cx: &mut App| {
                        let drag = event.drag(cx);
                        if drag.rail_id != drag_id_for_check {
                            return;
                        }
                        let relative_y: f32 =
                            (event.event.position.y - event.bounds.origin.y).into();
                        let thumb_top = (relative_y
                            - SCROLL_MAP_TRACK_VERTICAL_INSET
                            - drag.grab_offset_y.get())
                        .clamp(0.0, viewport_travel);
                        cx.stop_propagation();
                        drag_cb(thumb_top_to_position(thumb_top), true, cx);
                    },
                )
                .on_mouse_down(
                    MouseButton::Left,
                    move |event: &MouseDownEvent, _window, cx| {
                        cx.stop_propagation();
                        let Some(bounds) = click_track_bounds.get() else {
                            return;
                        };
                        let relative_y: f32 = (event.position.y - bounds.origin.y).into();
                        if relative_y >= viewport_top
                            && relative_y <= viewport_top + viewport_height
                        {
                            return;
                        }
                        let clicked_position = (relative_y / track_height).clamp(0.0, 1.0);
                        let position = clicked_position - visible_portion / 2.0;
                        let position = if position >= free_portion {
                            1.0
                        } else {
                            position.max(0.0)
                        };
                        click_cb(position, false, cx);
                    },
                )
        })
        .child(
            inner
                .w(px(SCROLL_MAP_RAIL_WIDTH))
                .group_hover("scroll-map-inline", |style| {
                    style.w(px(SCROLL_MAP_GUTTER_WIDTH))
                }),
        )
        .into_any_element()
}
