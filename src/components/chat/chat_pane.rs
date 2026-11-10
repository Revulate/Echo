//! Chat pane rendering for active channel splits.
//!
//! This module handles the UI for rendering a chat pane with messages,
//! header, and optional input area.
#![allow(clippy::too_many_arguments)]

use crate::EchoApp;
use crate::backend::StreamInfo;
use crate::backend::irc::room_state::RoomState;
use crate::components::chat::chat_header_renderer::{PinnedHeaderToggle, main_split_header_height};
use crate::components::chat::{ChatHeaderRenderer, ChatInputRenderer};
use crate::components::overlays::{
    clear_global_tooltip, clear_global_tooltip_if_source_in_window, show_text_tooltip,
};
use crate::components::ui::ButtonExt;
use crate::managers::{OverlayManager, SplitLayoutManager};
use crate::state::{ReplyState, SplitId, is_placeholder};
use crate::theme::colors;
use crate::theme::tokens::{FontSize, Radii, RadiiPixels, Spacing};
use gpui::prelude::FluentBuilder;
use gpui::*;

use crate::log::RENDER_TRACE_VERBOSE;

/// 14px scroll-map gutter plus an 8px trailing breathing margin. The pin
/// bubble never reaches into this interactive rail.
const PINNED_CHAT_BUBBLE_RIGHT_CLEARANCE_PX: f32 = 22.0;
const PINNED_CHAT_BUBBLE_MAX_WIDTH_PX: f32 = 420.0;
const PINNED_CHAT_BUBBLE_MAX_EXPANDED_HEIGHT_PX: f32 = 180.0;
/// The 28px collapsed pin plus its 2px top inset stays above the message viewport. An expanded pin may
/// intentionally overlay below this retained affordance strip.
pub(crate) const PINNED_CHAT_COLLAPSED_STRIP_HEIGHT_PX: f32 = 30.0;

#[derive(Clone)]
pub struct SplitLeafViewHandles {
    pub app: WeakEntity<EchoApp>,
    pub chat_input_renderer: Entity<ChatInputRenderer>,
    pub chat_header_renderer: Entity<ChatHeaderRenderer>,
    pub overlay_manager: Entity<OverlayManager>,
    pub chat_backend: Entity<crate::state::ChatBackend>,
}

#[derive(Clone)]
pub struct SplitLeafRenderParams {
    pub split_id: SplitId,
    pub channel: String,
    pub path: Vec<usize>,
    pub pane_width: f32,
    pub pane_height: f32,
    pub input_height: f32,
    pub search_height: f32,
    pub highlights_enabled: bool,
    pub show_split_input: bool,
    pub drag_mode: bool,
    pub is_active_split: bool,
    pub stream_info: Option<StreamInfo>,
    pub room_state: Option<RoomState>,
    pub split_settings_open: bool,
    pub compact_split_header: bool,
    pub header_collapsed: bool,
    /// The active tab's split layout is locked, so structure controls hide.
    pub layout_locked: bool,
    pub mod_mode: bool,
    pub input_focus: FocusHandle,
    pub reply_state: Option<ReplyState>,
}

/// Cheap change key for everything that feeds [`SplitLeafRenderParams`].
///
/// Every field here is either a scalar the parent already has or a version /
/// entity identity, so the parent can decide whether a leaf moved *before*
/// materializing the owned snapshot (stream info clone, room-state lock,
/// channel/path clones). `room_state_version` is global rather than per-channel:
/// a bump for an unrelated channel only costs one extra reconcile, never a
/// missed one.
#[derive(Clone, Copy, PartialEq)]
pub struct SplitLeafParamsGate {
    pub pane_width: f32,
    pub pane_height: f32,
    pub input_height: f32,
    pub search_height: f32,
    pub highlights_enabled: bool,
    pub show_split_input: bool,
    pub drag_mode: bool,
    pub is_active_split: bool,
    pub split_settings_open: bool,
    pub compact_split_header: bool,
    pub header_collapsed: bool,
    pub layout_locked: bool,
    pub mod_mode: bool,
    pub room_state_version: u64,
    pub stream_info_hash: u64,
    /// Whole minutes the stream has been live (0 while offline). The header
    /// prints uptime in minutes, so the retained leaf rebuilds exactly when
    /// that label changes even though the stream info itself did not.
    pub uptime_minutes: i64,
    /// Bumps only when an open channel's cached Twitch pin changes. The pinned
    /// bubble reads the snapshot itself, while this gate keeps the retained
    /// split from staying stale after an async Helix refresh completes.
    pub pinned_chat_version: u64,
    /// Split-local mutation key for a bubble dismissal or unpin reset. This
    /// makes the retained leaf rebuild immediately even though the pin cache
    /// itself has not changed.
    pub pinned_chat_dismissal_version: u64,
    /// Split-local mutation key for the expanded/collapsed pin bubble. This
    /// lets the chevron update a retained leaf immediately without waiting for
    /// a cache refresh.
    pub pinned_chat_expansion_version: u64,
    /// The bubble reads this privacy state while rendering, so retained leaves
    /// must invalidate when streamer mode toggles.
    pub streamer_mode_active: bool,
    pub reply_state_hash: u64,
    pub split_pane_id: Option<EntityId>,
}

/// Hash exactly the [`StreamInfo`] fields `stream_info_eq` compares.
pub fn stream_info_gate_hash(info: Option<&StreamInfo>) -> u64 {
    crate::backend::stream_info_gate_hash(info)
}

/// Hash exactly the [`ReplyState`] fields `reply_state_eq` compares.
pub fn reply_state_gate_hash(state: Option<&ReplyState>) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = ahash::AHasher::default();
    match state {
        None => 0u8.hash(&mut hasher),
        Some(state) => {
            1u8.hash(&mut hasher);
            state.message_id.hash(&mut hasher);
            state.username.hash(&mut hasher);
            state.preview.hash(&mut hasher);
        }
    }
    hasher.finish()
}

impl SplitLeafRenderParams {
    fn visible_eq(&self, other: &Self) -> bool {
        self.split_id == other.split_id
            && self.channel == other.channel
            && self.path == other.path
            && (self.pane_width - other.pane_width).abs() <= 0.5
            && (self.pane_height - other.pane_height).abs() <= 0.5
            && (self.input_height - other.input_height).abs() <= 0.5
            && (self.search_height - other.search_height).abs() <= 0.5
            && self.highlights_enabled == other.highlights_enabled
            && self.show_split_input == other.show_split_input
            && self.drag_mode == other.drag_mode
            && self.is_active_split == other.is_active_split
            && stream_info_eq(&self.stream_info, &other.stream_info)
            && self.room_state == other.room_state
            && self.split_settings_open == other.split_settings_open
            && self.compact_split_header == other.compact_split_header
            && self.header_collapsed == other.header_collapsed
            && self.layout_locked == other.layout_locked
            && self.mod_mode == other.mod_mode
            && self.input_focus == other.input_focus
            && reply_state_eq(&self.reply_state, &other.reply_state)
    }
}

fn stream_info_eq(a: &Option<StreamInfo>, b: &Option<StreamInfo>) -> bool {
    match (a, b) {
        (Some(a), Some(b)) => {
            a.title == b.title
                && a.game_name == b.game_name
                && a.viewer_count == b.viewer_count
                && a.started_at == b.started_at
                && a.is_live == b.is_live
        }
        (None, None) => true,
        _ => false,
    }
}

fn reply_state_eq(a: &Option<ReplyState>, b: &Option<ReplyState>) -> bool {
    match (a, b) {
        (Some(a), Some(b)) => {
            a.message_id == b.message_id && a.username == b.username && a.preview == b.preview
        }
        (None, None) => true,
        _ => false,
    }
}

/// Retained, SplitId-keyed render boundary for steady-state chat leaves.
///
/// Zed keeps pane contents behind cached retained view entities. Echo keeps
/// transient split affordances in `EchoApp` and renders the expensive chat leaf
/// shell here so parent split-tree refreshes can reuse GPUI prepaint/paint state.
pub struct SplitLeafView {
    handles: SplitLeafViewHandles,
    params: SplitLeafRenderParams,
    gate: SplitLeafParamsGate,
    empty_chat: Option<EmptyChatWatch>,
    _composer_observer: Subscription,
}

/// What an empty chat pane says while its buffer has no rows yet.
#[derive(Clone, Copy, PartialEq, Eq)]
enum EmptyChatState {
    Joining,
    Loading,
    /// Joined, but recent history is taking longer than a moment.
    LoadingSlow,
    NoMessages,
    /// Joined, and the recent-messages pass never reported back.
    HistoryUnavailable,
    NotFound,
    Suspended,
}

/// Fallback for a joined channel whose recent-messages pass never reports
/// back: after this it reads as quiet instead of loading.
const EMPTY_CHAT_LOADING_GRACE: std::time::Duration = std::time::Duration::from_secs(20);
/// After this the pane says the channel is live rather than only "loading".
const EMPTY_CHAT_LOADING_SLOW: std::time::Duration = std::time::Duration::from_secs(3);
const EMPTY_CHAT_RECHECK: std::time::Duration = std::time::Duration::from_millis(500);
const EMPTY_CHAT_RECHECK_MAX: std::time::Duration = std::time::Duration::from_secs(5);
/// A not-found or suspended channel can come back, so it is still rechecked,
/// slowly.
const EMPTY_CHAT_RECHECK_UNAVAILABLE: std::time::Duration = std::time::Duration::from_secs(30);

/// Watches one empty channel buffer until its first row arrives.
struct EmptyChatWatch {
    channel: String,
    state: Option<EmptyChatState>,
    _observer: Option<Subscription>,
    _recheck: Task<()>,
}

fn chat_state_entity(
    handles: &SplitLeafViewHandles,
    channel: &str,
    cx: &App,
) -> Option<Entity<crate::state::ChatState>> {
    handles.chat_backend.read(cx).get_channel(channel)
}

fn empty_chat_state(
    channel: &str,
    chat: Option<&Entity<crate::state::ChatState>>,
    connected_at: &mut Option<std::time::Instant>,
    cx: &App,
) -> Option<EmptyChatState> {
    use crate::backend::twitch_api::{ChannelLookupState, channel_lookup_state};
    if chat.is_some_and(|chat| !chat.read(cx).messages.is_empty()) {
        return None;
    }
    match channel_lookup_state(channel) {
        ChannelLookupState::NotFound => return Some(EmptyChatState::NotFound),
        ChannelLookupState::Suspended => return Some(EmptyChatState::Suspended),
        ChannelLookupState::Unknown | ChannelLookupState::Found => {}
    }
    if !crate::backend::irc::is_channel_join_confirmed(channel) {
        *connected_at = None;
        return Some(EmptyChatState::Joining);
    }
    let elapsed = connected_at
        .get_or_insert_with(std::time::Instant::now)
        .elapsed();
    Some(if crate::backend::irc::is_history_loaded(channel) {
        EmptyChatState::NoMessages
    } else if elapsed >= EMPTY_CHAT_LOADING_GRACE {
        EmptyChatState::HistoryUnavailable
    } else if elapsed >= EMPTY_CHAT_LOADING_SLOW {
        EmptyChatState::LoadingSlow
    } else {
        EmptyChatState::Loading
    })
}

/// Whether an empty-pane status can no longer change without a new message,
/// so its recheck timer can stop.
fn empty_chat_state_settled(channel: &str, state: Option<EmptyChatState>, observed: bool) -> bool {
    match state {
        None => true,
        Some(EmptyChatState::NoMessages) => {
            observed
                && crate::backend::twitch_api::channel_lookup_state(channel)
                    == crate::backend::twitch_api::ChannelLookupState::Found
        }
        Some(
            EmptyChatState::Joining
            | EmptyChatState::Loading
            | EmptyChatState::LoadingSlow
            | EmptyChatState::HistoryUnavailable
            | EmptyChatState::NotFound
            | EmptyChatState::Suspended,
        ) => false,
    }
}

impl SplitLeafView {
    pub fn new(
        handles: SplitLeafViewHandles,
        gate: SplitLeafParamsGate,
        params: SplitLeafRenderParams,
        cx: &mut Context<Self>,
    ) -> Self {
        // The composer is drawn inside this cached leaf, but its state lives on
        // the shared, view-less `ChatInputRenderer`. Its notifies (history
        // recall, cursor keys, deletes, paste, caret blink) dirty no view, so
        // the leaf must repaint itself or the edit waits for an unrelated redraw.
        let composer_observer = cx.observe(&handles.chat_input_renderer, |view, _, cx| {
            if view.params.show_split_input && view.params.is_active_split {
                cx.notify();
            }
        });
        let mut view = Self {
            handles,
            params,
            gate,
            empty_chat: None,
            _composer_observer: composer_observer,
        };
        view.sync_empty_chat_watch(cx);
        view
    }

    fn observe_empty_chat(
        chat: &Entity<crate::state::ChatState>,
        cx: &mut Context<Self>,
    ) -> Subscription {
        cx.observe(chat, |view, chat, cx| {
            if !chat.read(cx).messages.is_empty()
                && let Some(watch) = view.empty_chat.take()
                && watch.state.is_some()
            {
                cx.notify();
            }
        })
    }

    fn sync_empty_chat_watch(&mut self, cx: &mut Context<Self>) {
        let channel = self.params.channel.clone();
        if self
            .empty_chat
            .as_ref()
            .is_some_and(|watch| watch.channel == channel)
        {
            return;
        }
        self.empty_chat = None;
        if is_placeholder(&channel) || crate::backend::whispers::is_whispers_channel(&channel) {
            return;
        }
        let chat = chat_state_entity(&self.handles, &channel, cx);
        let mut connected_at = None;
        let Some(state) = empty_chat_state(&channel, chat.as_ref(), &mut connected_at, cx) else {
            return;
        };
        let observer = chat.as_ref().map(|chat| Self::observe_empty_chat(chat, cx));
        let recheck_channel = channel.clone();
        let recheck = cx.spawn(async move |this, cx| {
            let mut delay = EMPTY_CHAT_RECHECK;
            loop {
                cx.background_executor().timer(delay).await;
                let keep_watching = this
                    .update(cx, |view, cx| {
                        if view
                            .empty_chat
                            .as_ref()
                            .is_none_or(|watch| watch.channel != recheck_channel)
                        {
                            return false;
                        }
                        let chat = chat_state_entity(&view.handles, &recheck_channel, cx);
                        let next = empty_chat_state(
                            &recheck_channel,
                            chat.as_ref(),
                            &mut connected_at,
                            cx,
                        );
                        let Some(watch) = view.empty_chat.as_mut() else {
                            return false;
                        };
                        if watch._observer.is_none()
                            && let Some(chat) = chat.as_ref()
                        {
                            watch._observer = Some(Self::observe_empty_chat(chat, cx));
                        }
                        if next != watch.state {
                            watch.state = next;
                            delay = EMPTY_CHAT_RECHECK;
                            cx.notify();
                        } else if matches!(
                            next,
                            Some(EmptyChatState::NotFound | EmptyChatState::Suspended)
                        ) {
                            delay = EMPTY_CHAT_RECHECK_UNAVAILABLE;
                        } else {
                            delay = (delay * 2).min(EMPTY_CHAT_RECHECK_MAX);
                        }
                        !empty_chat_state_settled(&recheck_channel, next, watch._observer.is_some())
                    })
                    .unwrap_or(false);
                if !keep_watching {
                    return;
                }
            }
        });
        self.empty_chat = Some(EmptyChatWatch {
            channel,
            state: Some(state),
            _observer: observer,
            _recheck: recheck,
        });
    }

    /// Whether the retained snapshot already reflects `gate`, so the parent can
    /// skip building a fresh [`SplitLeafRenderParams`] entirely. Channel and
    /// path are compared against the retained snapshot instead of being hashed
    /// into the gate: both are already borrowed at the call site.
    pub fn gate_matches(&self, gate: &SplitLeafParamsGate, channel: &str, path: &[usize]) -> bool {
        self.gate == *gate && self.params.channel == channel && self.params.path == path
    }

    pub fn update_params(
        &mut self,
        gate: SplitLeafParamsGate,
        params: SplitLeafRenderParams,
        cx: &mut Context<Self>,
    ) {
        let gate_changed = self.gate != gate;
        self.gate = gate;
        if self.params.visible_eq(&params) {
            if gate_changed {
                cx.notify();
            }
            return;
        }
        self.params = params;
        self.sync_empty_chat_watch(cx);
        cx.notify();
    }
}

impl Render for SplitLeafView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if *RENDER_TRACE_VERBOSE {
            crate::log_perf_render!(
                debug,
                channel = %self.params.channel,
                path = ?self.params.path,
                split_id = %self.params.split_id,
                "split_leaf.render"
            );
        }
        let empty_chat = self.empty_chat.as_ref().and_then(|watch| watch.state);
        render_split_leaf_contents(&self.handles, &self.params, empty_chat, window, cx)
    }
}

fn render_split_leaf_contents<T: 'static>(
    handles: &SplitLeafViewHandles,
    params: &SplitLeafRenderParams,
    empty_chat: Option<EmptyChatState>,
    window: &mut Window,
    cx: &mut Context<T>,
) -> AnyElement {
    let Some(app_entity) = handles.app.upgrade() else {
        return div().size_full().into_any_element();
    };

    let input_height = if params.show_split_input {
        params.input_height
    } else {
        0.0
    };
    let header_height = if params.header_collapsed {
        0.0
    } else {
        main_split_header_height(params.compact_split_header)
    };
    let header_input_height = header_height + params.search_height + input_height;
    let docked_search = if params.search_height > 0.0 {
        let search = handles.overlay_manager.read(cx).search_modal.clone();
        Some(search.update(cx, |search, cx| {
            search.render_docked(
                &params.channel,
                params.pane_width,
                handles.app.clone(),
                window,
                cx,
            )
        }))
    } else {
        None
    };
    let split_pane = app_entity
        .read(cx)
        .split_panes
        .get(&params.split_id)
        .cloned();
    let pinned_chat_bubble = render_pinned_chat_bubble(
        &app_entity,
        split_pane.clone(),
        params.split_id,
        &params.channel,
        params.pane_width,
        params.pane_height,
        header_height,
        params.search_height,
        params.header_collapsed,
        cx,
    );
    let pinned_header_toggle = pinned_header_toggle(split_pane, &params.channel, cx);
    let pinned_chat_strip_height = if pinned_chat_bubble.is_some() {
        PINNED_CHAT_COLLAPSED_STRIP_HEIGHT_PX
    } else {
        0.0
    };
    let message_list_height =
        (params.pane_height - header_input_height - pinned_chat_strip_height).max(0.0);
    let empty_chat_notice = empty_chat
        .filter(|_| message_list_height > 40.0)
        .map(|state| {
            render_empty_chat_notice(
                &params.channel,
                state,
                header_height + params.search_height + pinned_chat_strip_height,
                message_list_height,
            )
        });

    div()
        .id("chat-pane")
        .w(px(params.pane_width))
        .h(px(params.pane_height))
        .min_h_0()
        .min_w_0()
        .flex_shrink_0()
        .flex()
        .flex_col()
        .overflow_hidden()
        .relative()
        .when(!params.header_collapsed, |pane| {
            pane.child(div().h(px(header_height)).flex_shrink_0())
        })
        .children(docked_search)
        .when(pinned_chat_strip_height > 0.0, |pane| {
            pane.child(div().h(px(pinned_chat_strip_height)).flex_shrink_0())
        })
        .child(div().flex_1().min_h_0().min_w_0())
        .when(params.show_split_input, |s| {
            let input_elem = handles.chat_input_renderer.update(cx, |renderer, cx| {
                renderer.render(
                    &params.channel,
                    &params.path,
                    &params.input_focus,
                    params.is_active_split,
                    params.split_id,
                    params.reply_state.clone(),
                    params.room_state.as_ref(),
                    params.pane_width,
                    app_entity.clone(),
                    handles.overlay_manager.clone(),
                    window,
                    cx,
                )
            });
            s.child(input_elem)
        })
        .when(!params.header_collapsed, |pane| {
            pane.child({
                let header = handles
                    .chat_header_renderer
                    .update(cx, |renderer, header_cx| {
                        renderer.render_fast(
                            &params.channel,
                            &params.path,
                            params.pane_width,
                            params.stream_info.as_ref(),
                            false,
                            params.drag_mode,
                            params.split_settings_open,
                            params.compact_split_header,
                            params.is_active_split,
                            params.layout_locked,
                            params.mod_mode,
                            pinned_header_toggle,
                            app_entity.clone(),
                            handles.overlay_manager.clone(),
                            header_cx,
                        )
                    });

                div().absolute().top_0().left_0().right_0().child(header)
            })
        })
        .when(params.header_collapsed, |pane| {
            pane.child(render_collapsed_header_capsule(
                &app_entity,
                &handles.overlay_manager,
                &params.channel,
                &params.path,
                params.split_id,
                params.search_height,
                params.is_active_split,
                cx,
            ))
        })
        .children(empty_chat_notice)
        .children(pinned_chat_bubble)
        .into_any_element()
}

/// Centered status over an empty message list: joining, loading recent
/// messages, a quiet channel, or a channel Twitch doesn't have. It has no
/// listeners, so clicks reach the chat.
fn render_empty_chat_notice(
    channel: &str,
    state: EmptyChatState,
    top: f32,
    height: f32,
) -> AnyElement {
    let (text, hint) = match state {
        EmptyChatState::Joining => (format!("Joining #{channel}\u{2026}"), None),
        EmptyChatState::Loading => ("Loading recent messages\u{2026}".to_string(), None),
        EmptyChatState::LoadingSlow => (
            "Connected \u{2014} waiting for messages\u{2026}".to_string(),
            Some("Recent messages are still loading.".to_string()),
        ),
        EmptyChatState::NoMessages => ("No messages yet".to_string(), None),
        EmptyChatState::HistoryUnavailable => (
            "No messages yet".to_string(),
            Some("Connected. Recent messages could not be loaded.".to_string()),
        ),
        EmptyChatState::NotFound => (
            "Channel not found".to_string(),
            Some(format!("Check that #{channel} is spelled correctly.")),
        ),
        EmptyChatState::Suspended => (
            "Channel suspended".to_string(),
            Some(format!(
                "Twitch has suspended #{channel}. Check the name if you meant another channel."
            )),
        ),
    };
    div()
        .absolute()
        .top(px(top))
        .left_0()
        .right_0()
        .h(px(height))
        .flex()
        .items_center()
        .justify_center()
        .px_4()
        .child(
            div()
                .flex()
                .flex_col()
                .items_center()
                .gap_1()
                .child(
                    div()
                        .text_sm()
                        .text_color(colors::text_muted())
                        .text_center()
                        .child(text),
                )
                .children(hint.map(|hint| {
                    div()
                        .text_xs()
                        .text_color(colors::text_muted())
                        .text_center()
                        .child(hint)
                })),
        )
        .into_any_element()
}

/// Resolve the bounded pin cache from the chat-pane lifecycle and render its
/// split-local floating affordance. Header visibility never controls refresh:
/// a collapsed split can reopen with its current pin already cached.
fn render_pinned_chat_bubble<T: 'static>(
    app: &Entity<EchoApp>,
    split_pane: Option<Entity<crate::state::SplitPane>>,
    split_id: SplitId,
    channel: &str,
    pane_width: f32,
    pane_height: f32,
    header_height: f32,
    search_height: f32,
    header_collapsed: bool,
    cx: &mut Context<T>,
) -> Option<AnyElement> {
    if crate::backend::whispers::is_whispers_channel(channel) {
        return None;
    }

    crate::backend::pinned_chat::queue_refresh(channel);
    let pinned = crate::backend::pinned_chat::get_cached(channel);
    let split_pane = split_pane?;

    let Some(pinned) = pinned else {
        split_pane.update(cx, |pane, cx| {
            pane.clear_dismissed_pinned_chat(cx);
            if pane.clear_pinned_chat_expansion() {
                cx.notify();
            }
        });
        return None;
    };

    split_pane.update(cx, |pane, cx| {
        pane.sync_pinned_chat_expansion(
            &pinned.message_id,
            pinned.updated_at.as_deref(),
            &pinned.text,
            cx,
        );
    });

    if header_collapsed
        || split_pane.read(cx).is_pinned_chat_dismissed(
            &pinned.message_id,
            pinned.updated_at.as_deref(),
            &pinned.text,
        )
    {
        return None;
    }

    let max_width = (pane_width - 8.0 - PINNED_CHAT_BUBBLE_RIGHT_CLEARANCE_PX)
        .min(PINNED_CHAT_BUBBLE_MAX_WIDTH_PX);
    if max_width <= 0.0 {
        return None;
    }

    let redact = crate::backend::streamer_mode::is_streamer_mode_active();
    let expanded = !redact
        && split_pane.read(cx).is_pinned_chat_expanded(
            &pinned.message_id,
            pinned.updated_at.as_deref(),
            &pinned.text,
        );
    let max_expanded_height = (pane_height - header_height - search_height - 24.0)
        .clamp(32.0, PINNED_CHAT_BUBBLE_MAX_EXPANDED_HEIGHT_PX);
    let visible_label = if redact {
        "Message hidden".to_string()
    } else if pinned.sender_name.is_empty() {
        pinned.text.clone()
    } else if pinned.text.is_empty() {
        pinned.sender_name.clone()
    } else {
        format!("{}: {}", pinned.sender_name, pinned.text)
    };
    let tooltip = if redact {
        "Pinned chat message. Activate to jump to it when it is loaded.".to_string()
    } else if pinned.sender_name.is_empty() {
        format!("Pinned message: {}", pinned.text)
    } else if pinned.text.is_empty() {
        format!("Pinned message from {}", pinned.sender_name)
    } else {
        format!(
            "Pinned message from {}: {}",
            pinned.sender_name, pinned.text
        )
    };
    let accessible_label = if redact || pinned.sender_name.is_empty() {
        "Pinned chat message. Activate to jump to it when it is loaded.".to_string()
    } else {
        format!(
            "Pinned message from {}. Activate to jump to it when it is loaded.",
            pinned.sender_name
        )
    };
    let channel = channel.to_string();
    let message_id = pinned.message_id.clone();
    let updated_at = pinned.updated_at.clone();
    let text = pinned.text.clone();
    let jump_channel = channel.clone();
    let jump_message_id = message_id.clone();
    let bubble_tooltip_source = format!("pinned-chat-bubble-{}", split_id.as_u64());
    let close_tooltip_source = format!("{}-close", bubble_tooltip_source);
    let expansion_tooltip_source = format!("{}-expand", bubble_tooltip_source);
    let bubble_tooltip_source_for_hover = bubble_tooltip_source.clone();
    let close_tooltip_source_for_hover = close_tooltip_source.clone();
    let expansion_tooltip_source_for_hover = expansion_tooltip_source.clone();
    let tooltip_for_hover = tooltip.clone();
    let split_pane_for_close = split_pane.clone();
    let split_pane_for_expansion = split_pane.clone();
    let app_for_dismiss = app.clone();
    let app_for_expansion = app.clone();
    let expansion_message_id = message_id.clone();
    let expansion_updated_at = updated_at.clone();
    let expansion_text = text.clone();
    let expansion_action_label = if expanded {
        "Collapse pinned message"
    } else {
        "Show full pinned message"
    };
    let expansion_action_label_for_hover = expansion_action_label.to_string();

    Some(
        div()
            .id(("pinned-chat-bubble", split_id.as_u64()))
            .absolute()
            .top(px(header_height + search_height + 2.0))
            .left(px(8.0))
            .max_w(px(max_width))
            .min_w_0()
            .flex()
            .when(expanded, |bubble| {
                bubble
                    .max_h(px(max_expanded_height))
                    .items_start()
                    .overflow_hidden()
            })
            .when(!expanded, |bubble| {
                bubble.h(px(28.0)).items_center().overflow_hidden()
            })
            .overflow_hidden()
            .rounded(Radii::sm())
            .bg(colors::bg_elevated())
            .border_1()
            .border_color(colors::border_subtle())
            .child(
                div()
                    .id(("pinned-chat-bubble-body", split_id.as_u64()))
                    .when(!expanded, |body| body.h_full())
                    .min_h_0()
                    .min_w_0()
                    .flex_1()
                    .px(px(Spacing::SM))
                    .flex()
                    .gap(px(Spacing::XS_PLUS))
                    .when(expanded, |body| {
                        body.py(px(Spacing::SM)).items_start().overflow_y_scroll()
                    })
                    .when(!expanded, |body| body.items_center())
                    .cursor_pointer()
                    .role(Role::Button)
                    .aria_label(accessible_label)
                    .on_activate(move |_event, _window, cx| {
                        cx.stop_propagation();
                        crate::backend::push_ui_action(
                            crate::backend::UiAction::JumpToMessageInSplit {
                                split_id,
                                channel: jump_channel.clone(),
                                message_id: jump_message_id.clone(),
                            },
                        );
                    })
                    .on_hover(move |hovered, window, _app| {
                        if *hovered {
                            show_text_tooltip(
                                window.window_handle(),
                                bubble_tooltip_source_for_hover.clone(),
                                tooltip_for_hover.clone(),
                                window.mouse_position(),
                            );
                        } else {
                            clear_global_tooltip_if_source_in_window(
                                window.window_handle(),
                                &bubble_tooltip_source_for_hover,
                            );
                        }
                    })
                    .child(
                        div()
                            .flex_shrink_0()
                            .text_size(px(FontSize::XS))
                            .text_color(colors::text_muted())
                            .child("PINNED"),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .text_size(px(FontSize::SM))
                            .text_color(colors::text_secondary())
                            .when(expanded, |text| text.whitespace_normal())
                            .when(!expanded, |text| text.truncate().whitespace_nowrap())
                            .child(visible_label),
                    ),
            )
            .when(!redact, move |bubble| {
                bubble.child(
                    div()
                        .id(("pinned-chat-bubble-expand", split_id.as_u64()))
                        .size(px(24.0))
                        .flex_shrink_0()
                        .flex()
                        .items_center()
                        .justify_center()
                        .cursor_pointer()
                        .role(Role::Button)
                        .aria_label(expansion_action_label)
                        .hover(|style| style.bg(colors::bg_hover()))
                        .on_activate(move |_event, _window, cx| {
                            cx.stop_propagation();
                            clear_global_tooltip();
                            split_pane_for_expansion.update(cx, |pane, pane_cx| {
                                pane.toggle_pinned_chat_expansion(
                                    &expansion_message_id,
                                    expansion_updated_at.as_deref(),
                                    &expansion_text,
                                    pane_cx,
                                );
                            });
                            app_for_expansion.update(cx, |_app, app_cx| app_cx.notify());
                        })
                        .on_hover(move |hovered, window, _app| {
                            if *hovered {
                                show_text_tooltip(
                                    window.window_handle(),
                                    expansion_tooltip_source_for_hover.clone(),
                                    expansion_action_label_for_hover.clone(),
                                    window.mouse_position(),
                                );
                            } else {
                                clear_global_tooltip_if_source_in_window(
                                    window.window_handle(),
                                    &expansion_tooltip_source_for_hover,
                                );
                            }
                        })
                        .child(
                            svg()
                                .path(if expanded {
                                    "icons/chevron-up.svg"
                                } else {
                                    "icons/chevron-down.svg"
                                })
                                .size(px(14.0))
                                .text_color(colors::text_muted()),
                        ),
                )
            })
            .child(
                div()
                    .id(("pinned-chat-bubble-close", split_id.as_u64()))
                    .size(px(24.0))
                    .flex_shrink_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor_pointer()
                    .role(Role::Button)
                    .aria_label("Dismiss pinned message")
                    .hover(|style| style.bg(colors::bg_hover()))
                    .on_activate(move |_event, _window, cx| {
                        cx.stop_propagation();
                        clear_global_tooltip();
                        split_pane_for_close.update(cx, |pane, pane_cx| {
                            pane.dismiss_pinned_chat(
                                &message_id,
                                updated_at.as_deref(),
                                &text,
                                pane_cx,
                            );
                        });
                        app_for_dismiss.update(cx, |_app, app_cx| app_cx.notify());
                    })
                    .on_hover(move |hovered, window, _app| {
                        if *hovered {
                            show_text_tooltip(
                                window.window_handle(),
                                close_tooltip_source_for_hover.clone(),
                                "Dismiss pinned message".to_string(),
                                window.mouse_position(),
                            );
                        } else {
                            clear_global_tooltip_if_source_in_window(
                                window.window_handle(),
                                &close_tooltip_source_for_hover,
                            );
                        }
                    })
                    .child(
                        div()
                            .text_size(px(FontSize::XL))
                            .text_color(colors::text_muted())
                            .child(crate::icons::CLOSE_GLYPH),
                    ),
            )
            .into_any_element(),
    )
}

/// The header's pin toggle for this split, when the channel has a cached pin.
/// Call after [`render_pinned_chat_bubble`], which forgets a stale dismissal
/// once Twitch reports no pin.
fn pinned_header_toggle<T: 'static>(
    split_pane: Option<Entity<crate::state::SplitPane>>,
    channel: &str,
    cx: &Context<T>,
) -> Option<PinnedHeaderToggle> {
    if crate::backend::whispers::is_whispers_channel(channel) {
        return None;
    }
    let split_pane = split_pane?;
    let message = crate::backend::pinned_chat::get_cached(channel)?;
    let dismissed = split_pane.read(cx).is_pinned_chat_dismissed(
        &message.message_id,
        message.updated_at.as_deref(),
        &message.text,
    );
    Some(PinnedHeaderToggle {
        split_pane,
        message,
        dismissed,
    })
}

/// A small, always-reachable control for a leaf whose full channel header is
/// hidden. It is deliberately absolute: it cannot consume chat viewport space
/// and it never forwards a click to the composer/send path.
///
/// `search_height` is the reserved height of the docked search row (the same
/// value used to size that row). When the header is collapsed the docked search
/// occupies the top of the pane, so the capsule is pushed down by that amount to
/// sit at the top-right of the chat area instead of overlapping the search bar.
pub(crate) fn render_collapsed_header_capsule<T: 'static>(
    app: &Entity<EchoApp>,
    overlay_manager: &Entity<OverlayManager>,
    channel: &str,
    path: &[usize],
    split_id: SplitId,
    search_height: f32,
    is_active: bool,
    _cx: &mut Context<T>,
) -> impl IntoElement + use<T> {
    let path_key = SplitLayoutManager::path_key(path);
    let path: std::sync::Arc<[usize]> = path.into();
    let restore_path = path.clone();
    let menu_path = path;
    let app_for_restore = app.clone();
    let app_for_menu = app.clone();
    let overlay_for_menu = overlay_manager.clone();
    let channel: std::sync::Arc<str> = channel.into();
    let menu_channel = channel.clone();
    let restore_tooltip = format!("collapsed-header-restore-{}", split_id.as_u64());
    let menu_tooltip = format!("collapsed-header-actions-{}", split_id.as_u64());

    div()
        .id(("collapsed-header-capsule", split_id.as_u64()))
        .absolute()
        .top(px(search_height + 6.0))
        .right(px(8.0))
        .h(px(28.0))
        .w(px(60.0))
        .flex()
        .items_center()
        .overflow_hidden()
        .rounded(Radii::full())
        .bg(colors::bg_elevated())
        .border_1()
        .border_color(if is_active {
            colors::active_split_outline(colors::bg_elevated())
        } else {
            colors::border()
        })
        .shadow_sm()
        .child(
            div()
                .id(("restore-split-header", split_id.as_u64()))
                .h_full()
                .min_w_0()
                .flex_1()
                .flex()
                .items_center()
                .justify_center()
                .cursor_pointer()
                .role(Role::Button)
                .aria_label("Show header")
                .hover(|style| style.bg(colors::bg_hover()))
                .on_accessible_activate(move |_event, _window, cx| {
                    clear_global_tooltip();
                    app_for_restore.update(cx, |app, app_cx| {
                        app.set_split_header_collapsed(restore_path.to_vec(), false, app_cx);
                    });
                    cx.stop_propagation();
                })
                .on_hover({
                    let source = restore_tooltip.clone();
                    move |hovered, window, _app| {
                        if *hovered {
                            show_text_tooltip(
                                window.window_handle(),
                                source.clone(),
                                "Show header".to_string(),
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
                        .text_size(px(FontSize::XS))
                        .font_weight(FontWeight::SEMIBOLD)
                        .child("Show"),
                ),
        )
        .child(
            div()
                .id(("collapsed-header-actions", split_id.as_u64()))
                .h_full()
                .w(px(24.0))
                .flex()
                .items_center()
                .justify_center()
                .border_l_1()
                .border_color(colors::border_subtle())
                .cursor_pointer()
                .role(Role::Button)
                .aria_label("Split actions")
                .hover(|style| style.bg(colors::bg_hover()))
                .on_accessible_activate(move |_event, _window, cx| {
                    clear_global_tooltip();
                    app_for_menu.update(cx, |app, app_cx| {
                        app.set_active_split(menu_path.to_vec(), app_cx);
                    });
                    overlay_for_menu.update(cx, |overlays, _| {
                        overlays.toggle_split_settings(path_key.clone(), menu_channel.to_string());
                    });
                    app_for_menu.update(cx, |_, app_cx| app_cx.notify());
                    cx.stop_propagation();
                })
                .on_hover({
                    let source = menu_tooltip.clone();
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
                .child(div().text_size(px(FontSize::MD)).child("⋯")),
        )
}

/// Render a chat pane for a given channel.
///
/// This includes the header, message list, and optional input area.
pub fn render_chat_pane(
    app: &mut EchoApp,
    channel: &str,
    path: Vec<usize>,
    available_width: f32,
    available_height: f32,
    highlights_enabled: bool,
    header_collapsed: bool,
    window: &mut Window,
    cx: &mut Context<EchoApp>,
) -> AnyElement {
    let pane_width = available_width.max(0.0);
    let pane_height = available_height.max(0.0);

    if is_placeholder(channel) {
        return crate::components::splits::render_placeholder_pane(
            app,
            path,
            pane_width,
            pane_height,
            window,
            cx,
        );
    }

    let stream_info = app
        .app_state
        .read(cx)
        .stream_info
        .get(channel)
        .cloned()
        .flatten();
    let show_timestamp = crate::backend::settings::should_show_message_timestamps(
        stream_info.as_ref().is_some_and(|info| info.is_live),
    );

    let _channel_key = channel;

    let path_key = SplitLayoutManager::path_key(&path);
    let room_state = crate::backend::irc::room_state::get_room_state(channel);

    let (show_split_input, is_active, drag_mode) = {
        let split_layout = app.split_layout.read(cx);
        (
            !split_layout.is_unified_input_mode(),
            split_layout.get_active_split_path() == path,
            split_layout.is_drag_mode_active(),
        )
    };

    let Some(split_id) = app
        .app_state
        .read(cx)
        .active_tab()
        .layout
        .get_split_id_at_path(&path)
    else {
        return div().flex_1().min_h_0().min_w_0().into_any_element();
    };

    let (input_focus, reply_state) = app
        .split_panes
        .get(&split_id)
        .map(|pane| {
            let pane_data = pane.read(cx);
            (pane_data.focus_handle.clone(), pane_data.reply_to.clone())
        })
        .unwrap_or_else(|| (app.input_focus.clone(), None));

    let input_height = if show_split_input {
        app.chat_input_renderer
            .read(cx)
            .input_surface_metrics_for_split(split_id, reply_state.is_some())
            .total_height
    } else {
        0.0
    };
    let compact_split_header = crate::backend::settings::get_compact_split_header();
    let header_height = if header_collapsed {
        0.0
    } else {
        main_split_header_height(compact_split_header)
    };
    let search = app.overlay_manager.read(cx).search_modal.clone();
    let search_height = if is_active && search.read(cx).is_docked_open() {
        crate::components::overlays::DOCKED_SEARCH_HEIGHT
    } else {
        0.0
    };
    let header_input_height = header_height + search_height + input_height;
    let docked_search = if search_height > 0.0 {
        let owner = cx.entity().downgrade();
        Some(search.update(cx, |search, cx| {
            search.render_docked(channel, pane_width, owner, window, cx)
        }))
    } else {
        None
    };
    let pinned_chat_bubble = render_pinned_chat_bubble(
        &cx.entity(),
        app.split_panes.get(&split_id).cloned(),
        split_id,
        channel,
        pane_width,
        pane_height,
        header_height,
        search_height,
        header_collapsed,
        cx,
    );
    let pinned_header_toggle =
        pinned_header_toggle(app.split_panes.get(&split_id).cloned(), channel, cx);
    let pinned_chat_strip_height = if pinned_chat_bubble.is_some() {
        PINNED_CHAT_COLLAPSED_STRIP_HEIGHT_PX
    } else {
        0.0
    };
    let message_list_height =
        (pane_height - header_input_height - pinned_chat_strip_height).max(0.0);

    let click_path = path.clone();
    let click_split_id = split_id;

    div()
        .id("chat-pane")
        .w(px(pane_width))
        .h(px(pane_height))
        .min_h_0()
        .min_w_0()
        .flex_shrink_0()
        .flex()
        .flex_col()
        .overflow_hidden()
        .relative()
        .bg(colors::bg_void())
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |this, _event: &MouseDownEvent, window, cx| {
                this.set_active_split(click_path.clone(), cx);

                if let Some(pane) = this.split_panes.get(&click_split_id) {
                    let focus_handle = pane.read(cx).focus_handle.clone();
                    window.focus(&focus_handle, cx);
                }
            }),
        )
        .when(!header_collapsed, |pane| {
            pane.child(div().h(px(header_height)).flex_shrink_0())
        })
        .children(docked_search)
        .when(pinned_chat_strip_height > 0.0, |pane| {
            pane.child(div().h(px(pinned_chat_strip_height)).flex_shrink_0())
        })
        .child({
            if pane_width > 1.0 && message_list_height > 1.0 {
                let chat_view = app.get_or_create_chat_view(split_id, channel, pane_width, cx);
                chat_view.update(cx, |view, cx| {
                    view.update_render_params(
                        pane_width,
                        message_list_height,
                        highlights_enabled,
                        show_timestamp,
                        is_active,
                        cx,
                    );
                });

                if *RENDER_TRACE_VERBOSE {
                    crate::log_perf_render!(
                        debug,
                        channel = %channel,
                        path = ?path,
                        cached = true,
                        uncached_refresh = false,
                        dirty_reasons = "owned_by_chat_view",
                        dirty = false,
                        "chat_pane.render_chat_view"
                    );
                }
                let chat_view_element = AnyView::from(chat_view)
                    .cached(StyleRefinement::default().size_full())
                    .into_any_element();

                div()
                    .w_full()
                    .h(px(message_list_height))
                    .min_h_0()
                    .min_w_0()
                    .flex_shrink_0()
                    .relative()
                    .child(chat_view_element)
                    .into_any_element()
            } else {
                div().flex_1().min_h_0().min_w_0().into_any_element()
            }
        })
        .when(show_split_input, |s| {
            let app_entity = cx.entity().clone();
            let overlay_manager = app.overlay_manager.clone();
            let path_clone = path.clone();
            let pane_width = app.split_layout.read(cx).pane_width_for_path(&path);

            let input_elem = app.chat_input_renderer.update(cx, |renderer, cx| {
                renderer.render(
                    channel,
                    &path_clone,
                    &input_focus,
                    is_active,
                    split_id,
                    reply_state,
                    room_state.as_ref(),
                    pane_width,
                    app_entity,
                    overlay_manager,
                    window,
                    cx,
                )
            });
            s.child(input_elem)
        })
        .when(!header_collapsed, |pane| {
            pane.child({
                let settings_open = app
                    .overlay_manager
                    .read(cx)
                    .is_split_settings_open(path_key);
                let overlay_mgr = app.overlay_manager.clone();
                let app_entity = cx.entity().clone();
                let layout_locked = app.app_state.read(cx).active_tab().is_locked;
                let mod_mode = app.split_layout.read(cx).is_mod_mode(split_id);

                let header = app.chat_header_renderer.update(cx, |renderer, header_cx| {
                    renderer.render_fast(
                        channel,
                        &path,
                        pane_width,
                        stream_info.as_ref(),
                        false,
                        drag_mode,
                        settings_open,
                        compact_split_header,
                        is_active,
                        layout_locked,
                        mod_mode,
                        pinned_header_toggle,
                        app_entity,
                        overlay_mgr,
                        header_cx,
                    )
                });

                div().absolute().top_0().left_0().right_0().child(header)
            })
        })
        .when(header_collapsed, |pane| {
            pane.child(render_collapsed_header_capsule(
                &cx.entity(),
                &app.overlay_manager,
                channel,
                &path,
                split_id,
                search_height,
                is_active,
                cx,
            ))
        })
        .children(pinned_chat_bubble)
        .into_any_element()
}
