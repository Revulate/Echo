//! ChatView entity for rendering a single chat pane
//!
//! This extracts chat pane rendering from the app.rs monolith into a dedicated entity.

use crate::backend::ChatMessage;
use crate::components::chat::canvas_renderer::CanvasMessageList;
use crate::components::chat::scroll_map::{
    SCROLL_MAP_GUTTER_WIDTH, ScrollMapViewport, ScrollMarker, compute_scroll_markers,
    render_scroll_map_from_markers,
};
use crate::state::{AppState, ChatState, ChatStateEvent, SplitId};
use echo_core::reveal_pacer::RevealPacer;
use gpui::*;
use std::sync::Arc;

use crate::log::RENDER_TRACE_VERBOSE;

const INACTIVE_HISTORY_DEFER_MIN_OLD_MESSAGES: usize = 32;

#[inline]
fn canvas_message_width(pane_width: f32) -> f32 {
    (pane_width - SCROLL_MAP_GUTTER_WIDTH).max(1.0)
}

#[derive(Clone, Copy, Debug)]
pub enum ChatViewDirtyReason {
    Initial,
    ChatStateEvent,
    DecorationEvent,
    RenderParams,
    CanvasStateSync,
    ManualScroll,
    SearchJump,
    ScrollMapJump,
    InactiveHistoryDeferred,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct ChatViewDirtyReasons(u16);

impl ChatViewDirtyReasons {
    const INITIAL: u16 = 1 << 0;
    const CHAT_STATE_EVENT: u16 = 1 << 2;
    const RENDER_PARAMS: u16 = 1 << 3;
    const CANVAS_STATE_SYNC: u16 = 1 << 4;
    const MANUAL_SCROLL: u16 = 1 << 5;
    const SEARCH_JUMP: u16 = 1 << 6;
    const SCROLL_MAP_JUMP: u16 = 1 << 7;
    const DECORATION_EVENT: u16 = 1 << 8;
    const INACTIVE_HISTORY_DEFERRED: u16 = 1 << 9;

    fn insert(&mut self, reason: ChatViewDirtyReason) {
        self.0 |= Self::bit_for(reason);
    }

    fn bit_for(reason: ChatViewDirtyReason) -> u16 {
        match reason {
            ChatViewDirtyReason::Initial => Self::INITIAL,
            ChatViewDirtyReason::ChatStateEvent => Self::CHAT_STATE_EVENT,
            ChatViewDirtyReason::RenderParams => Self::RENDER_PARAMS,
            ChatViewDirtyReason::CanvasStateSync => Self::CANVAS_STATE_SYNC,
            ChatViewDirtyReason::ManualScroll => Self::MANUAL_SCROLL,
            ChatViewDirtyReason::SearchJump => Self::SEARCH_JUMP,
            ChatViewDirtyReason::ScrollMapJump => Self::SCROLL_MAP_JUMP,
            ChatViewDirtyReason::DecorationEvent => Self::DECORATION_EVENT,
            ChatViewDirtyReason::InactiveHistoryDeferred => Self::INACTIVE_HISTORY_DEFERRED,
        }
    }

    pub fn is_empty(self) -> bool {
        self.0 == 0
    }

    pub fn labels(self) -> &'static str {
        match self.0 {
            0 => "none",
            Self::INITIAL => "initial",
            Self::CHAT_STATE_EVENT => "chat_state_event",
            Self::RENDER_PARAMS => "render_params",
            Self::CANVAS_STATE_SYNC => "canvas_state_sync",
            Self::MANUAL_SCROLL => "manual_scroll",
            Self::SEARCH_JUMP => "search_jump",
            Self::SCROLL_MAP_JUMP => "scroll_map_jump",
            Self::DECORATION_EVENT => "decoration_event",
            Self::INACTIVE_HISTORY_DEFERRED => "inactive_history_deferred",
            _ => "multiple",
        }
    }
}

/// A paced prefix of a message list: (source list, visible count, prefix).
type RevealedPrefix = (
    Arc<Vec<Arc<ChatMessage>>>,
    usize,
    Arc<Vec<Arc<ChatMessage>>>,
);

/// Entity for rendering a single chat pane with messages, input, and header
pub struct ChatView {
    /// Channel name for this chat view
    pub channel: String,
    /// App state reference for accessing messages and settings
    pub app_state: Entity<AppState>,
    /// Channel-local reactive state for this specific chat view.
    pub chat_state: Entity<ChatState>,
    /// Canvas message list entity for high-performance rendering
    pub canvas_list: Entity<CanvasMessageList>,
    /// Cached scroll-map markers (recomputed only when messages/user change)
    cached_scroll_markers: Vec<ScrollMarker>,
    /// Stable identity used to route GPUI drag events to this split's scroll map.
    scroll_map_drag_id: SharedString,
    /// Last message buffer observed by scroll-map cache
    last_scroll_map_messages: Arc<Vec<Arc<ChatMessage>>>,
    /// Last username used for mention detection in scroll-map cache
    last_scroll_map_username: Option<String>,
    /// Last active search marker version used for scroll-map cache
    last_scroll_map_search_version: u64,
    /// Last width assigned by the parent split/popup.
    message_width: f32,
    /// Last viewport height assigned by the parent split/popup.
    viewport_height: f32,
    /// Split-local highlight enablement assigned by the parent.
    highlights_enabled: bool,
    /// Split-local timestamp visibility after global/live-channel policy.
    show_timestamp: bool,
    /// Whether this split is the active/focused split in its tab.
    is_active_split: bool,
    /// Diagnostics for why this split requested a cached ChatView refresh.
    dirty_reasons: ChatViewDirtyReasons,
    /// Latest history-prepend snapshot deferred while this split is inactive.
    deferred_inactive_messages: Option<Arc<Vec<Arc<ChatMessage>>>>,
    /// Holds the newest live rows back and releases them at a steady pace.
    reveal_pacer: RevealPacer,
    /// Last paced prefix, reused while neither the source list nor the
    /// visible count changed.
    reveal_prefix: Option<RevealedPrefix>,
    /// When the pending reveal wake fires, if one is scheduled.
    reveal_wake_at: Option<std::time::Instant>,
    reveal_wake_task: Option<Task<()>>,
    /// Keeps channel-local subscriptions alive for split-scoped redraws.
    _subscriptions: Vec<Subscription>,
}

impl ChatView {
    pub fn new(
        channel: String,
        split_id: SplitId,
        app_state: Entity<AppState>,
        chat_state: Entity<ChatState>,
        container_width: f32,
        cx: &mut Context<Self>,
    ) -> Self {
        let canvas_key = format!("{}:split-{}", channel, split_id.as_u64());
        let scroll_map_drag_id: SharedString =
            format!("scroll-map:{}:split-{}", channel, split_id.as_u64()).into();

        let canvas_list = cx.new(|cx| {
            CanvasMessageList::new(
                canvas_key,
                split_id,
                canvas_message_width(container_width),
                cx,
            )
        });
        let chat_state_subscription = cx.subscribe(
            &chat_state,
            |this, entity, event: &ChatStateEvent, cx| match event {
                ChatStateEvent::Added | ChatStateEvent::Replaced | ChatStateEvent::Retokenized => {
                    let messages = entity.read(cx).messages.clone();
                    if this.defer_inactive_history_prepend_if_needed(&messages, cx) {
                        return;
                    }
                    let message_width = canvas_message_width(this.message_width);
                    let viewport_height = this.viewport_height;
                    let highlights_enabled = this.highlights_enabled;
                    let show_timestamp = this.show_timestamp;
                    let is_active_split = this.is_active_split;
                    let changed = this.sync_canvas_list_state(
                        messages,
                        message_width,
                        viewport_height,
                        highlights_enabled,
                        show_timestamp,
                        is_active_split,
                        cx,
                    );
                    if changed {
                        this.mark_dirty_for(ChatViewDirtyReason::ChatStateEvent);
                        crate::backend::metrics::record_ui_notify_source(
                            "chat_view.chat_state_event",
                        );
                        cx.notify();
                    }
                }
                ChatStateEvent::UserDecorationsChanged {
                    user_login,
                    user_id,
                } => {
                    let changed = this.canvas_list.update(cx, |list, cx| {
                        let affected = list.invalidate_user_decoration_layouts(
                            user_login.as_ref(),
                            user_id.as_deref(),
                        );
                        if affected {
                            crate::backend::metrics::record_ui_notify_source(
                                "chat_view.user_decorations_changed",
                            );
                            cx.notify();
                        }
                        affected
                    });
                    if *RENDER_TRACE_VERBOSE {
                        crate::log_perf_render!(
                            debug,
                            channel = %this.channel,
                            user_login = %user_login,
                            user_id = ?user_id,
                            affected = changed,
                            "chat_view.user_decorations_changed"
                        );
                    }
                    if changed {
                        this.mark_dirty_for(ChatViewDirtyReason::DecorationEvent);
                        cx.notify();
                    }
                }
                ChatStateEvent::BadgeDecorationsChanged => {
                    let changed = this.canvas_list.update(cx, |list, cx| {
                        let affected = list.invalidate_badge_decorations();
                        if affected {
                            crate::backend::metrics::record_ui_notify_source(
                                "chat_view.badge_decorations_changed",
                            );
                            cx.notify();
                        }
                        affected
                    });
                    if changed {
                        this.mark_dirty_for(ChatViewDirtyReason::DecorationEvent);
                        cx.notify();
                    }
                }
            },
        );

        Self {
            channel,
            app_state,
            chat_state,
            canvas_list,
            cached_scroll_markers: Vec::new(),
            scroll_map_drag_id,
            last_scroll_map_messages: Arc::new(Vec::new()),
            last_scroll_map_username: None,
            last_scroll_map_search_version: 0,
            message_width: container_width,
            viewport_height: 600.0,
            highlights_enabled: true,
            show_timestamp: crate::backend::settings::get_show_message_timestamps(),
            is_active_split: true,
            dirty_reasons: {
                let mut reasons = ChatViewDirtyReasons::default();
                reasons.insert(ChatViewDirtyReason::Initial);
                reasons
            },
            deferred_inactive_messages: None,
            reveal_pacer: RevealPacer::new(),
            reveal_prefix: None,
            reveal_wake_at: None,
            reveal_wake_task: None,
            _subscriptions: vec![chat_state_subscription],
        }
    }

    /// Update render parameters owned by the parent split or popup.
    ///
    /// ChatView is mounted as its own entity so channel-local updates can repaint
    /// this split without requiring EchoApp to rebuild every split subtree.
    pub fn update_render_params(
        &mut self,
        message_width: f32,
        viewport_height: f32,
        highlights_enabled: bool,
        show_timestamp: bool,
        is_active_split: bool,
        cx: &mut Context<Self>,
    ) {
        let width_changed = (self.message_width - message_width).abs() > 0.5;
        let height_changed = (self.viewport_height - viewport_height).abs() > 0.5;
        let highlights_changed = self.highlights_enabled != highlights_enabled;
        let timestamp_changed = self.show_timestamp != show_timestamp;
        let active_changed = self.is_active_split != is_active_split;

        if width_changed
            || height_changed
            || highlights_changed
            || timestamp_changed
            || active_changed
        {
            self.message_width = message_width;
            self.viewport_height = viewport_height;
            self.highlights_enabled = highlights_enabled;
            self.show_timestamp = show_timestamp;
            self.is_active_split = is_active_split;
            let becoming_active = active_changed && is_active_split;
            self.canvas_list.update(cx, |list, cx| {
                list.set_active(is_active_split);
                if timestamp_changed {
                    list.show_timestamp = show_timestamp;
                    list.invalidate_local_layouts();
                }
                if becoming_active {
                    list.revalidate_on_show();
                    cx.notify();
                }
            });
            let deferred_changed = if becoming_active {
                self.apply_deferred_inactive_messages(cx)
            } else {
                false
            };

            if width_changed
                || height_changed
                || highlights_changed
                || timestamp_changed
                || deferred_changed
                || active_changed
            {
                self.mark_dirty_for(ChatViewDirtyReason::RenderParams);
                cx.notify();
            }
        }
    }

    /// Show or hide inline moderation buttons (moderation mode) for this split.
    pub fn set_mod_mode(&mut self, enabled: bool, cx: &mut Context<Self>) {
        let changed = self
            .canvas_list
            .update(cx, |list, _| list.set_mod_mode(enabled));
        if changed {
            self.mark_dirty_for(ChatViewDirtyReason::RenderParams);
            self.canvas_list.update(cx, |_, cx| cx.notify());
            cx.notify();
        }
    }

    /// Override how this split presents moderator-removed rows; `None` follows
    /// the global setting.
    pub fn set_deleted_mode(
        &mut self,
        mode: Option<crate::backend::settings::DeletedMessageMode>,
        cx: &mut Context<Self>,
    ) {
        let changed = self
            .canvas_list
            .update(cx, |list, _| list.set_deleted_mode_override(mode));
        if changed {
            self.mark_dirty_for(ChatViewDirtyReason::RenderParams);
            self.canvas_list.update(cx, |_, cx| cx.notify());
            cx.notify();
        }
    }

    /// Record the newest message as this split's last read position.
    pub fn mark_last_read(&mut self, cx: &mut Context<Self>) {
        self.canvas_list.update(cx, |list, _| list.mark_last_read());
    }

    /// Prime retained offscreen state before this split becomes visible again.
    ///
    /// Used by tab-hover prewarming so switching back to a retained tab can reuse
    /// already-requested layouts instead of waiting for the first activation frame.
    pub fn prewarm_for_activation(&mut self, cx: &mut Context<Self>) -> usize {
        let messages = self.get_messages(cx);
        let messages = self.messages_for_render(messages, cx);
        self.sync_canvas_list_state(
            messages,
            canvas_message_width(self.message_width),
            self.viewport_height,
            self.highlights_enabled,
            self.show_timestamp,
            self.is_active_split,
            cx,
        );
        self.canvas_list
            .update(cx, |list, _cx| list.prewarm_visible_layouts())
    }

    pub fn mark_dirty_for(&mut self, reason: ChatViewDirtyReason) {
        self.dirty_reasons.insert(reason);
    }

    pub fn take_dirty_reasons(&mut self) -> ChatViewDirtyReasons {
        std::mem::take(&mut self.dirty_reasons)
    }

    /// Get the current message snapshot for this channel from app state.
    fn get_messages(&self, cx: &mut Context<Self>) -> Arc<Vec<Arc<ChatMessage>>> {
        if crate::backend::blocked_users::resident_visibility_revoked() {
            return Arc::new(Vec::new());
        }
        if crate::backend::blocked_users::chat_state_reconcile_pending(&self.channel) {
            return Arc::new(
                crate::backend::irc::visible_channel_messages(&self.channel)
                    .into_iter()
                    .map(Arc::new)
                    .collect(),
            );
        }
        self.chat_state.read(cx).messages.clone()
    }

    fn inactive_history_prepend_info(
        &self,
        messages: &Arc<Vec<Arc<ChatMessage>>>,
        cx: &mut Context<Self>,
    ) -> Option<(usize, usize)> {
        if self.is_active_split {
            return None;
        }

        let canvas = self.canvas_list.read(cx);
        let old_messages = canvas.source_messages().as_ref();
        let new_messages = messages.as_ref();
        let old_count = old_messages.len();
        let new_count = new_messages.len();
        if old_count < INACTIVE_HISTORY_DEFER_MIN_OLD_MESSAGES || new_count <= old_count {
            return None;
        }

        let added_count = new_count - old_count;
        let old_order_matches_suffix = old_messages
            .iter()
            .zip(new_messages.iter().skip(added_count))
            .all(|(old_msg, new_msg)| old_msg.id.as_ref() == new_msg.id.as_ref());
        old_order_matches_suffix.then_some((added_count, old_count))
    }

    fn defer_inactive_history_prepend_if_needed(
        &mut self,
        messages: &Arc<Vec<Arc<ChatMessage>>>,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some((prepended_count, old_count)) = self.inactive_history_prepend_info(messages, cx)
        else {
            return false;
        };

        self.deferred_inactive_messages = Some(messages.clone());
        self.mark_dirty_for(ChatViewDirtyReason::InactiveHistoryDeferred);
        crate::log_perf_render!(
            debug,
            channel = %self.channel,
            prepended = prepended_count,
            old_count,
            "chat_view.deferred_inactive_history_prepend"
        );
        true
    }

    fn messages_for_render(
        &mut self,
        messages: Arc<Vec<Arc<ChatMessage>>>,
        cx: &mut Context<Self>,
    ) -> Arc<Vec<Arc<ChatMessage>>> {
        if self.defer_inactive_history_prepend_if_needed(&messages, cx) {
            return self.canvas_list.read(cx).source_messages().clone();
        }
        self.deferred_inactive_messages = None;
        messages
    }

    fn apply_deferred_inactive_messages(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(messages) = self.deferred_inactive_messages.take() else {
            return false;
        };

        let changed = self.sync_canvas_list_state(
            messages,
            canvas_message_width(self.message_width),
            self.viewport_height,
            self.highlights_enabled,
            self.show_timestamp,
            self.is_active_split,
            cx,
        );
        if changed {
            self.mark_dirty_for(ChatViewDirtyReason::InactiveHistoryDeferred);
        }
        changed
    }

    /// The part of `messages` this split shows now.
    ///
    /// Twitch can deliver a busy channel in bursts (dozens of messages every
    /// few seconds). While the split follows live chat, the pacer holds the
    /// newest rows back and releases them at the channel's arrival rate, so
    /// chat moves steadily; a wake is scheduled for the next release. The
    /// user's own messages and system rows show at once with everything
    /// before them. Scrolled-up, paused or hidden splits show everything.
    fn paced_messages(
        &mut self,
        messages: Arc<Vec<Arc<ChatMessage>>>,
        cx: &mut Context<Self>,
    ) -> Arc<Vec<Arc<ChatMessage>>> {
        let pace = crate::backend::settings::get_paced_chat_reveal() && {
            let canvas = self.canvas_list.read(cx);
            canvas.is_tab_visible() && canvas.is_following_live()
        };
        let visible = if pace {
            let own_login = self.app_state.read(cx).username.clone();
            let outcome = self.reveal_pacer.pace(
                messages.as_slice(),
                std::time::Instant::now(),
                |message| &message.id,
                |message| {
                    message.flags.is_system
                        || own_login
                            .as_deref()
                            .is_some_and(|login| message.sender.eq_ignore_ascii_case(login))
                },
            );
            if let Some(wake) = outcome.next_wake {
                self.schedule_reveal_wake(wake, cx);
            }
            outcome.visible
        } else {
            self.reveal_pacer
                .reveal_all(messages.as_slice(), |message| &message.id)
        };

        if visible >= messages.len() {
            self.reveal_prefix = None;
            return messages;
        }
        if let Some((source, count, prefix)) = &self.reveal_prefix
            && Arc::ptr_eq(source, &messages)
            && *count == visible
        {
            return prefix.clone();
        }
        let prefix = Arc::new(messages[..visible].to_vec());
        self.reveal_prefix = Some((messages, visible, prefix.clone()));
        prefix
    }

    /// Re-sync the canvas after `delay` so held rows keep releasing even when
    /// no new message arrives. An earlier pending wake is kept.
    fn schedule_reveal_wake(&mut self, delay: std::time::Duration, cx: &mut Context<Self>) {
        let due = std::time::Instant::now() + delay;
        if self.reveal_wake_at.is_some_and(|at| at <= due) {
            return;
        }
        self.reveal_wake_at = Some(due);
        self.reveal_wake_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(delay).await;
            let _ = this.update(cx, |this, cx| {
                this.reveal_wake_at = None;
                let messages = this.get_messages(cx);
                if this.defer_inactive_history_prepend_if_needed(&messages, cx) {
                    return;
                }
                let changed = this.sync_canvas_list_state(
                    messages,
                    canvas_message_width(this.message_width),
                    this.viewport_height,
                    this.highlights_enabled,
                    this.show_timestamp,
                    this.is_active_split,
                    cx,
                );
                if changed {
                    crate::backend::metrics::record_ui_notify_source("chat_view.paced_reveal");
                    cx.notify();
                }
            });
        }));
    }

    #[allow(clippy::too_many_arguments)]
    fn sync_canvas_list_state(
        &mut self,
        messages: Arc<Vec<Arc<ChatMessage>>>,
        message_width: f32,
        viewport_height: f32,
        highlights_enabled: bool,
        show_timestamp: bool,
        is_active_split: bool,
        cx: &mut Context<Self>,
    ) -> bool {
        let messages = self.paced_messages(messages, cx);
        let changed = self.canvas_list.update(cx, |list, cx| {
            let old_q = crate::components::chat::canvas_renderer::layout::quantize_width(
                list.container_width,
            );
            let new_q =
                crate::components::chat::canvas_renderer::layout::quantize_width(message_width);
            let width_changed = old_q != new_q;
            let height_changed = (list.viewport_height - viewport_height).abs() > 5.0;
            let msg_changed = !Arc::ptr_eq(list.source_messages(), &messages);
            let highlights_changed = list.highlights_enabled != highlights_enabled;
            let timestamp_changed = list.show_timestamp != show_timestamp;
            let _active_changed = list.set_active(is_active_split);

            list.set_container_width(message_width);
            list.set_viewport_height(viewport_height);
            if msg_changed {
                list.set_messages(messages);
            }

            if highlights_changed {
                list.highlights_enabled = highlights_enabled;
                list.invalidate_local_layouts();
            }
            if timestamp_changed {
                list.show_timestamp = show_timestamp;
                list.invalidate_local_layouts();
            }

            if msg_changed
                || width_changed
                || height_changed
                || highlights_changed
                || timestamp_changed
            {
                crate::backend::metrics::record_ui_notify_source(
                    "chat_view.sync_canvas_list_state.changed",
                );
                cx.notify();
            }
            msg_changed
                || width_changed
                || height_changed
                || highlights_changed
                || timestamp_changed
        });
        if changed {
            self.deferred_inactive_messages = None;
            self.mark_dirty_for(ChatViewDirtyReason::CanvasStateSync);
        }
        changed
    }

    /// Render just the message list (for use in render_chat_pane)
    ///
    /// Includes the scroll map on the right edge with:
    /// - Colored markers for mentions, subscriptions, first messages
    /// - Viewport position indicator
    /// - Click-to-navigate using the painted track's window-space bounds
    pub fn render_message_list_element(
        &mut self,
        message_width: f32,
        viewport_height: f32,
        highlights_enabled: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let messages = self.get_messages(cx);
        let messages = self.messages_for_render(messages, cx);
        let canvas_width = canvas_message_width(message_width);
        let message_list = self.render_messages(
            &messages,
            canvas_width,
            viewport_height,
            highlights_enabled,
            cx,
        );

        // The rail shares the canvas's index space, which leaves out rows this
        // split's deleted-message mode hides.
        let (messages, (top_position, bottom_position), new_message_boundary_position) = {
            let canvas = self.canvas_list.read(cx);
            (
                Arc::clone(canvas.messages()),
                canvas.index_space_viewport(),
                canvas.new_message_boundary_position(),
            )
        };

        let our_username = self.app_state.read(cx).username.clone();
        let username_changed = self.last_scroll_map_username != our_username;
        let messages_changed = !Arc::ptr_eq(&self.last_scroll_map_messages, &messages);
        let search_marker_version = crate::backend::search::active_search_markers_version();
        let search_markers_changed = self.last_scroll_map_search_version != search_marker_version;

        if messages_changed || username_changed || search_markers_changed {
            let search_result_ids = crate::backend::search::active_search_marker_ids(&self.channel);
            let search_result_ids = if search_result_ids.is_empty() {
                None
            } else {
                Some(search_result_ids)
            };

            self.cached_scroll_markers = compute_scroll_markers(
                messages.as_slice(),
                our_username.as_deref(),
                search_result_ids.as_ref(),
            );
            self.last_scroll_map_messages = messages.clone();
            self.last_scroll_map_username = our_username.clone();
            self.last_scroll_map_search_version = search_marker_version;
        }

        let scroll_map_viewport = ScrollMapViewport {
            new_message_boundary_position,
            top_position,
            bottom_position,
            visible_height: viewport_height,
        };

        let chat_view = cx.entity();
        let on_jump = Some(
            Arc::new(move |position: f32, dragging: bool, cx: &mut App| {
                chat_view.update(cx, |view, cx| {
                    view.canvas_list.update(cx, |list, cx| {
                        list.scroll_to_index_position(position, !dragging);
                        crate::backend::metrics::record_ui_notify_source(if dragging {
                            "canvas.scroll_map_drag"
                        } else {
                            "canvas.scroll_map_jump"
                        });
                        cx.notify();
                    });
                    view.mark_dirty_for(ChatViewDirtyReason::ScrollMapJump);
                    cx.notify();
                });
            }) as Arc<dyn Fn(f32, bool, &mut App) + Send + Sync + 'static>,
        );

        let scroll_map = render_scroll_map_from_markers(
            &self.cached_scroll_markers,
            messages.as_slice(),
            scroll_map_viewport,
            self.scroll_map_drag_id.clone(),
            on_jump,
        );

        div()
            .id("chat-content-row")
            .flex()
            .flex_row()
            .w(px(message_width))
            .h(px(viewport_height))
            .min_h_0()
            .min_w_0()
            .overflow_hidden()
            .child(message_list)
            .child(scroll_map)
            .into_any_element()
    }

    /// Render the message list using canvas renderer
    fn render_messages(
        &mut self,
        messages: &Arc<Vec<Arc<ChatMessage>>>,
        message_width: f32,
        viewport_height: f32,
        highlights_enabled: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        self.sync_canvas_list_state(
            messages.clone(),
            message_width,
            viewport_height,
            highlights_enabled,
            self.show_timestamp,
            self.is_active_split,
            cx,
        );

        AnyView::from(self.canvas_list.clone())
            .cached(
                StyleRefinement::default()
                    .flex_1()
                    .h_full()
                    .min_h_0()
                    .min_w_0(),
            )
            .into_any_element()
    }
}

impl Render for ChatView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if *RENDER_TRACE_VERBOSE {
            let dirty_reasons = self.take_dirty_reasons();
            crate::log_perf_render!(
                debug,
                channel = %self.channel,
                messages = self.chat_state.read(cx).messages.len(),
                message_width = self.message_width,
                viewport_height = self.viewport_height,
                dirty_reasons = %dirty_reasons.labels(),
                dirty = !dirty_reasons.is_empty(),
                "chat_view.render"
            );
        }
        if self.message_width <= 1.0 || self.viewport_height <= 1.0 {
            return div().size_full().min_h_0().min_w_0().into_any_element();
        }

        self.render_message_list_element(
            self.message_width,
            self.viewport_height,
            self.highlights_enabled,
            cx,
        )
    }
}
