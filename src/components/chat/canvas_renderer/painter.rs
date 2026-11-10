//! Canvas-based message list painter
//!
//! High-performance message rendering with multi-message selection and tooltips.
//! Uses element-local coordinates for proper hit testing across splits.
#![allow(clippy::too_many_arguments)]
#![allow(clippy::type_complexity)]

use super::global_layout_cache;
use super::layout::LayoutCacheKey;
use super::relayout_trace::{self, RelayoutCause, VisibleGeometryTrace};
use echo_render_core::eviction_cache::EvictionTrackingCache;
use gpui::{HighlightStyle, StyledText, *};
use rustc_hash::{FxHashMap, FxHasher};
use smallvec::SmallVec;
use std::cell::RefCell;
use std::hash::{Hash, Hasher};
use std::num::NonZeroUsize;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::LazyLock;

use crate::backend::ChatMessage;
use crate::cache::loader as image_cache;
use crate::emote_renderer;
use crate::managers::frame_rate_controller;
use crate::theme::colors;

use super::layout::{
    CanvasMessageLayout, EmbedPlatform, HitboxKind, ImagePaintCommand, LayoutSegment, OverlayEmote,
    SELECTION_COLOR, ShapedLineData, TextPaintCommand, is_animated_url, is_svg_image_url,
    line_height, message_content_fingerprint, quantize_width, text_height, width_from_quantized,
};
use super::text_shaping::build_message_layout;
use super::tooltip::TooltipState;
use crate::backend::settings;
use crate::components::overlays::{
    ContextMenuState, clear_global_tooltip_if_source, clear_global_tooltip_if_source_in_window,
    show_canvas_hitbox_tooltip,
};
use echo_chat_render::font_metrics::scaled_font_size;
use echo_chat_render::hit_testing::HitboxManager;
use echo_chat_render::selection::{SelectionPoint, SelectionRanges, SelectionState};
use echo_chat_render::width_bucket::{WIDTH_BUCKET_FORCE_APPLY_MS, WIDTH_BUCKET_STABILIZE_MS};

mod accessibility;
mod async_layouts;
#[cfg(feature = "gpui-bench")]
mod benchmark_hooks;
mod deleted_display;
mod direct_paint;
mod element_images;
mod element_render;
mod element_widgets;
mod height_anchor;
mod image_paint;
mod interaction;
mod layout_cache;
mod layout_invalidation;
mod layout_perf;
mod layout_pipeline;
mod layout_resolution;
mod layout_store;
mod media_preload;
mod message_delta;
mod messages;
mod model;
mod perf;
mod preload;
mod render;
mod scroll;
mod stale_refresh;
mod state;
mod text_paint;
mod trace;
mod viewport_element;
mod visible_rows;
mod wake;
mod width_bucket;

use self::deleted_display::DeletedDisplayState;
use self::direct_paint::{
    ViewportPaintRow, can_paint_visible_row_in_viewport_canvas, prepare_viewport_paint_row,
    reuse_prepared_viewport_paint_row,
};
use self::element_render::{ElementRenderPlan, FallbackRowFrame, render_message_at_position};
use self::layout_store::LayoutStore;
use self::model::MessageListModel;
use self::perf::{
    REBUILD_REASON_FORCE_REFRESH, REBUILD_REASON_HEIGHT_MISMATCH, REBUILD_REASON_ORDER_MISMATCH,
    REBUILD_REASON_STALE_LAYOUTS, RenderPerfAggregateStats, RenderPerfFrameStats,
    RowPaintPathTally, element_fallback_reasons_label, rebuild_reason_flags_label,
    stable_gate_failures_label,
};
use self::state::{
    AnimationSlotSource, InteractionState, LayoutRuntimeState, MessageLifecycleState,
    PaintedAnimationSlots, PendingTextClick, PreloadState, ScrollController, SelectionGranularity,
    UNSLOTTED_IMAGE_STATE_SHIFT, UNSLOTTED_NAME_PAINT, ViewportFrameState, WakeState,
};
use self::trace::{CanvasRenderTraceGuard, RENDER_TRACE_ENABLED, SCROLL_SMOKE_TRACE_ENABLED};
use self::viewport_element::render_visible_rows_viewport_element;
use self::visible_rows::{ScrollDirection, VisibleRenderRow};
use self::wake::AnimationPatchOutcome;

/// Splits narrower than this show the jump-to-live pill as a bare arrow.
const JUMP_TO_LIVE_COMPACT_WIDTH: f32 = 200.0;

/// A diagnostic timer that avoids reading the platform clock when its consumer
/// is disabled. Keeping this in the painter module makes the steady-state branch
/// consistent across render, visible-row, and paint-emission instrumentation.
pub(super) struct RenderTimer(Option<std::time::Instant>);

impl RenderTimer {
    #[inline]
    pub(super) fn start(enabled: bool) -> Self {
        Self(enabled.then(std::time::Instant::now))
    }

    #[inline]
    pub(super) fn elapsed(&self) -> std::time::Duration {
        self.0
            .as_ref()
            .map(std::time::Instant::elapsed)
            .unwrap_or_default()
    }
}

/// Canvas-based message list renderer (GPUI Entity)
pub struct CanvasMessageList {
    /// Split-local layout cache and invalidation/width-bucket runtime state.
    layout_store: LayoutStore,
    /// Message storage, display order, lookup index, measured heights, and total height.
    message_model: MessageListModel,
    /// How this split presents moderator-removed rows, and the shared snapshot
    /// `message_model` is projected from.
    deleted_display: DeletedDisplayState,

    /// Pointer, hit-test, selection, tooltip, cursor, and hover state.
    pub(in crate::components::chat::canvas_renderer) interaction_state: InteractionState,
    /// User-scroll pause and smooth-scroll animation state.
    pub(in crate::components::chat::canvas_renderer) scroll_controller: ScrollController,
    /// Container width
    pub container_width: f32,
    /// Viewport height (updated from element bounds)
    pub viewport_height: f32,

    /// Channel ID (for badge lookups and unique element ID)
    pub channel_id: String,
    /// Channel name without workspace prefix (cached for context menu)
    pub channel_name: String,
    /// Stable owning split for source-aware message context-menu actions.
    source_split_id: crate::state::SplitId,
    /// Cached ElementId to avoid format!() allocation every frame
    cached_element_id: ElementId,
    /// Whether to show timestamps
    pub show_timestamp: bool,
    /// Whether highlights are enabled (mentions, whispers, etc.)
    pub highlights_enabled: bool,
    /// Whether this split is the focused split in its tab.
    is_active_split: bool,
    /// Scale factor of the window this canvas last rendered in.
    layout_scale_factor: f32,
    /// Whether the split is in moderation mode.
    mod_mode: bool,
    /// Moderation mode while the viewer can moderate this channel, so rows
    /// carry inline moderation buttons.
    mod_buttons_active: bool,
    /// Whether this split belongs to the tab currently mounted in the main window.
    ///
    /// Retained inactive-tab entities keep their layout/message caches, but must not
    /// keep detached animation wake loops alive after leaving the rendered tree.
    is_tab_visible: bool,

    /// Focus handle for keyboard events
    pub focus_handle: FocusHandle,

    /// Message-buffer lifecycle and layout-rebuild flags.
    message_state: MessageLifecycleState,

    /// Message currently emphasized after opening a search result.
    search_focus_message_id: Option<SharedString>,
    /// Generation counter for ignoring stale search-focus clear timers.
    search_focus_generation: u64,

    /// Visible-range preload cursors, throttles, and embed fetch deduplication.
    preload_state: PreloadState,
    pub async_layout_manager: Arc<super::async_layout::AsyncLayoutManager>,

    /// Per-frame visible-row and prepared-paint snapshots.
    viewport_state: ViewportFrameState,
    /// Split-local wake-loop state for animations, async layouts, image loads, and tooltip delays.
    wake_state: WakeState,
    /// Batched render counter flushed into metrics periodically.
    split_render_flush_counter: u32,
    /// Per-frame render/layout counters collected during update_layouts/render.
    render_perf_frame_stats: RenderPerfFrameStats,
    /// Periodically flushed aggregate render/layout counters.
    render_perf_agg_stats: RenderPerfAggregateStats,
    /// Last time aggregated render/layout diagnostics were logged.
    last_render_perf_log: std::time::Instant,
}

impl CanvasMessageList {
    /// Create new canvas message list
    pub fn new(
        channel_id: String,
        source_split_id: crate::state::SplitId,
        container_width: f32,
        cx: &mut App,
    ) -> Self {
        crate::log_ui!(debug, channel_id = %channel_id, container_width, "CanvasMessageList::new");
        let channel_name = channel_id
            .split(':')
            .next()
            .unwrap_or(&channel_id)
            .to_string();
        let cached_element_id = ElementId::Name(format!("canvas-list-{}", channel_id).into());
        let tooltip_overlay_source_id = format!("canvas-tooltip-{}", channel_id);
        let layout_scale_factor = crate::backend::settings::get_window_scale_factor();
        Self {
            layout_store: LayoutStore::initial(container_width, layout_scale_factor),
            message_model: MessageListModel::initial(),
            deleted_display: DeletedDisplayState::initial(),
            interaction_state: InteractionState::initial(tooltip_overlay_source_id),
            scroll_controller: ScrollController::initial(),
            container_width,
            viewport_height: 600.0,
            channel_id,
            channel_name,
            source_split_id,
            cached_element_id,
            show_timestamp: true,
            highlights_enabled: true,
            is_active_split: true,
            layout_scale_factor,
            mod_mode: false,
            mod_buttons_active: false,
            is_tab_visible: true,
            focus_handle: cx.focus_handle(),
            message_state: MessageLifecycleState::initial(),
            search_focus_message_id: None,
            search_focus_generation: 0,
            preload_state: PreloadState::initial(),
            async_layout_manager: Arc::new(super::async_layout::AsyncLayoutManager::new()),
            viewport_state: ViewportFrameState::initial(),
            wake_state: WakeState::default(),
            split_render_flush_counter: 0,
            render_perf_frame_stats: RenderPerfFrameStats::default(),
            render_perf_agg_stats: RenderPerfAggregateStats::default(),
            last_render_perf_log: std::time::Instant::now()
                .checked_sub(std::time::Duration::from_secs(10))
                .unwrap_or_else(std::time::Instant::now),
        }
    }

    #[inline]
    pub fn messages(&self) -> &Arc<Vec<Arc<ChatMessage>>> {
        &self.message_model.messages
    }

    #[inline]
    pub fn total_height(&self) -> f32 {
        self.message_model.total_height
    }

    #[inline]
    pub fn scroll_offset(&self) -> f32 {
        self.scroll_controller.offset
    }

    /// Current smooth-scroll target used by benchmark diagnostics to prove
    /// that an injected scroll command requested real viewport movement.
    #[inline]
    pub fn scroll_target(&self) -> f32 {
        self.scroll_controller.state.scroll_desired
    }

    #[inline]
    pub fn viewport_height(&self) -> f32 {
        self.viewport_height
    }

    /// Exact message IDs intersecting the viewport, excluding layout overdraw.
    /// Activity uses this to clear only mentions the user could actually see.
    ///
    /// Borrowed: Activity harvests this at UI-poll cadence, so membership tests
    /// must not cost one owned string per visible row per pass.
    pub fn visible_message_ids(&self) -> &[SharedString] {
        if !self.is_tab_visible || self.message_model.height_tree.is_empty() {
            return &[];
        }
        let (start, end) = self.message_model.height_tree.visible_range(
            self.scroll_controller.offset,
            self.viewport_height,
            0.0,
        );
        self.message_model
            .message_order
            .get(start..end)
            .unwrap_or(&[])
    }

    /// Cheap change key for the visible-row set consumed by Activity read tracking.
    ///
    /// O(1): the message buffer `Arc` is only replaced when its contents change
    /// (see `sync_canvas_list_state`), and the eviction base plus row count move
    /// with every `message_order` mutation, so visible ids never need hashing.
    pub fn visibility_generation(&self) -> u64 {
        let (start, end) = if self.is_tab_visible && !self.message_model.height_tree.is_empty() {
            self.message_model.height_tree.visible_range(
                self.scroll_controller.offset,
                self.viewport_height,
                0.0,
            )
        } else {
            (0, 0)
        };

        let mut hasher = FxHasher::default();
        (Arc::as_ptr(&self.message_model.messages).cast::<()>() as usize).hash(&mut hasher);
        self.message_model.message_index_base.hash(&mut hasher);
        self.message_model.message_order.len().hash(&mut hasher);
        start.hash(&mut hasher);
        end.hash(&mut hasher);
        self.is_tab_visible.hash(&mut hasher);
        self.is_live_following().hash(&mut hasher);
        hasher.finish()
    }

    /// New tail rows are visible only while this mounted split follows live chat.
    pub fn is_live_following(&self) -> bool {
        self.is_tab_visible && self.is_at_bottom()
    }

    #[inline]
    pub(in crate::components::chat::canvas_renderer) fn sync_total_height_from_tree(&mut self) {
        self.message_model.sync_total_height();
    }

    /// Copy selected text to clipboard (for hotkey action)
    /// Returns true if text was copied, false if no selection
    pub fn copy_selection_to_clipboard(&self) -> bool {
        self.interaction_state
            .selection
            .copy_to_clipboard_with(&self.message_model.message_order, |message_id| {
                self.selection_layout(message_id)
            })
    }

    fn selection_layout(&self, message_id: &SharedString) -> Option<Arc<CanvasMessageLayout>> {
        let exact_key = self.layout_cache_key_from_shared(message_id.clone());
        self.peek_cached_layout(&exact_key)
            .or_else(|| self.find_any_layout_for_message(message_id))
    }

    /// Render the opt-in reply affordance for one visible message.
    fn render_reply_button(
        &self,
        message_id: &str,
        scroll_offset: f32,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let idx = self.get_message_index(message_id)?;
        let msg = self.get_message(message_id)?;
        let is_whisper = matches!(
            msg.flags.highlight_type.as_ref(),
            Some(crate::backend::HighlightType::Whisper)
        );
        if !is_whisper && !msg.allows_remote_message_actions() {
            return None;
        }
        let cache_key = LayoutCacheKey {
            message_id: SharedString::from(Arc::clone(&msg.id)),
            quantized_width: self.layout_quantized_width_for_cache(),
            show_timestamp: self.show_timestamp,
        };
        let layout = self.peek_cached_layout(&cache_key)?;

        let msg_y = self.message_model.height_tree.y_position(idx);
        let viewport_y = msg_y - scroll_offset;

        if viewport_y < -layout.height || viewport_y > self.viewport_height {
            return None;
        }

        let channel = self.channel_name.clone();
        let msg_id_owned = message_id.to_string();
        let sender = msg.sender.clone();
        let display_name = msg.display_name.clone();
        let content = msg.content.clone();

        let button_x = self.container_width - 32.0;
        let button_y = viewport_y + 4.0;

        Some(
            div()
                .id(ElementId::Name(
                    format!("message-reply-btn-{message_id}").into(),
                ))
                .absolute()
                .left(px(button_x))
                .top(px(button_y))
                .w(px(24.0))
                .h(px(24.0))
                .rounded(px(4.0))
                .bg(colors::bg_hover())
                .border_1()
                .border_color(colors::border())
                .cursor_pointer()
                .flex()
                .items_center()
                .justify_center()
                .hover(|s| s.bg(colors::bg_elevated()))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _event: &MouseDownEvent, _window, cx| {
                        use crate::backend;
                        if is_whisper {
                            backend::push_ui_action(backend::UiAction::ComposeWhisper {
                                username: sender.to_string(),
                                channel: Some(channel.clone()),
                            });
                        } else {
                            backend::push_ui_action(backend::UiAction::Reply {
                                channel: channel.clone(),
                                source_split_id: Some(this.source_split_id),
                                message_id: msg_id_owned.clone(),
                                sender: sender.to_string(),
                                display_name: display_name.to_string(),
                                content: content.to_string(),
                            });
                        }
                        cx.stop_propagation();
                        crate::backend::metrics::record_ui_notify_source(
                            "canvas.reply_button_click",
                        );
                        cx.notify();
                    }),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(colors::text_secondary())
                        .child("↩"),
                )
                .into_any_element(),
        )
    }

    fn render_jump_to_live(&self, cx: &mut Context<Self>) -> AnyElement {
        use gpui::prelude::FluentBuilder;

        let compact = self.container_width < JUMP_TO_LIVE_COMPACT_WIDTH;
        let label: SharedString = if compact {
            "↓".into()
        } else if let Some(new_count) = self.new_messages_below_count() {
            format!("↓ {new_count} new").into()
        } else {
            "↓ Jump to live".into()
        };
        let pill = div()
            .id(ElementId::Name("chat-jump-to-live".into()))
            .h(px(30.0))
            .when(compact, |pill| pill.w(px(30.0)))
            .when(!compact, |pill| pill.px(px(12.0)))
            .rounded(px(15.0))
            .flex()
            .items_center()
            .justify_center()
            .gap(px(6.0))
            .bg(colors::bg_elevated().opacity(0.96))
            .border_1()
            .border_color(colors::accent().opacity(0.72))
            .shadow_md()
            .cursor_pointer()
            .hover(|style| style.bg(colors::bg_hover()).border_color(colors::accent()))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _event: &MouseDownEvent, _window, cx| {
                    this.scroll_to_bottom();
                    cx.stop_propagation();
                    crate::backend::metrics::record_ui_notify_source("canvas.jump_to_live_click");
                    cx.notify();
                }),
            )
            .child(
                div()
                    .text_sm()
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(colors::text_primary())
                    .child(label),
            );
        div()
            .absolute()
            .left_0()
            .right_0()
            .bottom(px(12.0))
            .flex()
            .justify_center()
            .child(pill)
            .into_any_element()
    }

    fn new_messages_below_count(&self) -> Option<usize> {
        let boundary_id = self.message_state.new_message_boundary_id.as_ref()?;
        let index = self.get_message_index(boundary_id.as_ref())?;
        let count = self.message_model.message_order.len().saturating_sub(index);
        (count > 0).then_some(count)
    }

    /// Get visible range of messages based on scroll (O(log n) with HeightTree)
    pub(super) fn get_visible_range(&self) -> (usize, usize) {
        let active_scroll =
            self.scroll_controller.state.scroll_animating || self.is_user_scrolling();
        self.get_visible_range_for_offset(self.scroll_controller.offset, active_scroll)
    }

    pub(super) fn get_visible_range_for_offset(
        &self,
        scroll_offset: f32,
        active_scroll: bool,
    ) -> (usize, usize) {
        if self.message_model.height_tree.is_empty() {
            return (0, 0);
        }

        let overdraw = if active_scroll {
            (self.viewport_height * 0.25).clamp(75.0, 300.0)
        } else {
            (self.viewport_height * 0.10).clamp(30.0, 120.0)
        };

        self.message_model
            .height_tree
            .visible_range(scroll_offset, self.viewport_height, overdraw)
    }

    /// Get the exact viewport range without overdraw.
    ///
    /// Overdraw rows may be measured or painted ahead of time, but anchoring
    /// scroll compensation to the true viewport start prevents those offscreen
    /// height corrections from moving the visible top edge.
    pub(super) fn get_viewport_range(&self) -> (usize, usize) {
        if self.message_model.height_tree.is_empty() {
            return (0, 0);
        }

        self.message_model.height_tree.visible_range(
            self.scroll_controller.offset,
            self.viewport_height,
            0.0,
        )
    }
}

/// The "Paused" badge shown while auto-scroll is held by a pause modifier.
fn render_pause_indicator() -> AnyElement {
    div()
        .absolute()
        .right(px(12.0))
        .bottom(px(12.0))
        .px(px(8.0))
        .py(px(4.0))
        .rounded(px(4.0))
        .bg(colors::bg_elevated().opacity(0.92))
        .border_1()
        .border_color(colors::accent_transparent(0.28))
        .shadow_sm()
        .child(
            div()
                .text_xs()
                .font_weight(FontWeight::MEDIUM)
                .text_color(colors::accent_hover())
                .child("Paused"),
        )
        .into_any_element()
}

/// Stops this split's wake loops and releases the process-wide state it
/// registered (animation census, global tooltip) when the split closes.
impl Drop for CanvasMessageList {
    fn drop(&mut self) {
        if let Some(owner) = self.wake_state.animation_census_owner {
            crate::emote_renderer::remove_owner(owner);
        }
        if self.wake_state.animation_waker_running {
            self.release_animation_waker();
        }
        if self.wake_state.async_layout_waker_running {
            self.release_async_layout_waker();
        }
        if self.wake_state.image_load_waker_running {
            self.release_image_load_waker();
        }
        if self.wake_state.width_stabilization_waker_running {
            self.release_width_stabilization_waker();
        }
        clear_global_tooltip_if_source(&self.interaction_state.tooltip_overlay_source_id);

        let entries_released = self.layout_store.layouts.len()
            + self.message_model.message_index.len()
            + self.layout_store.runtime.cached_width_index.len()
            + self.preload_state.pending_embed_fetches.len();

        crate::log_ui!(debug,
            channel_id = %self.channel_id,
            entries_released,
            "CanvasMessageList dropped"
        );
    }
}
