//! Per-channel chat state and GPUI reactivity facade.

use crate::backend::ChatMessage;
use echo_core::chat_snapshot::ChatSnapshot;
use gpui::*;
use std::ops::Deref;
use std::sync::Arc;

#[derive(Clone, Debug)]
pub enum ChatStateEvent {
    Added,
    Replaced,
    Retokenized,
    UserDecorationsChanged {
        user_login: Arc<str>,
        user_id: Option<Arc<str>>,
    },
    BadgeDecorationsChanged,
}

#[derive(Clone)]
pub struct ChatState {
    snapshot: ChatSnapshot,
}

#[derive(Clone, Debug)]
pub struct ReplyState {
    pub message_id: String,
    pub username: String,
    pub preview: String,
}

impl Default for ChatState {
    fn default() -> Self {
        Self::new(String::new())
    }
}

impl std::fmt::Debug for ChatState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChatState")
            .field("channel", &self.channel)
            .field("message_count", &self.messages.len())
            .finish()
    }
}

impl Deref for ChatState {
    type Target = ChatSnapshot;

    fn deref(&self) -> &Self::Target {
        &self.snapshot
    }
}

impl EventEmitter<ChatStateEvent> for ChatState {}

impl ChatState {
    pub fn new(channel: String) -> Self {
        Self {
            snapshot: ChatSnapshot::new(
                channel,
                crate::state::message_retention::active_channel_capacity(),
            ),
        }
    }

    pub fn update_messages_reactive(
        &mut self,
        new_messages: Vec<ChatMessage>,
        cx: &mut Context<Self>,
    ) -> bool {
        if new_messages.is_empty() {
            return false;
        }

        if !self.snapshot.append_messages_to_snapshot(new_messages) {
            return false;
        }
        cx.emit(ChatStateEvent::Added);
        cx.notify();
        true
    }

    pub fn replace_messages_reactive(
        &mut self,
        messages: Vec<ChatMessage>,
        cx: &mut Context<Self>,
    ) -> bool {
        let changed = self.snapshot.merge_history_into_snapshot(messages);
        if !changed {
            crate::log_irc!(
                trace,
                "replace_messages_reactive: history snapshot for '{}' did not change",
                self.channel
            );
            return false;
        }

        cx.emit(ChatStateEvent::Replaced);
        cx.notify();
        true
    }

    pub fn refresh_from_backend_reactive(&mut self, cx: &mut Context<Self>) -> bool {
        let refresh_started = std::time::Instant::now();
        let old_message_count = self.messages.len();
        let old_messages_ptr = Arc::as_ptr(&self.messages);
        let old_channel_emote_version =
            crate::state::VersionRegistry::channel_emote_version(&self.channel);

        let channel_lower = self.channel.to_lowercase();
        let blocked_reconcile_generation =
            crate::backend::blocked_users::chat_state_reconcile_generation(&channel_lower);
        let backend_messages = crate::backend::irc::visible_channel_messages(&channel_lower);
        if backend_messages.is_empty()
            && !crate::backend::irc::CHAT_MESSAGES.contains_key(&channel_lower)
            && blocked_reconcile_generation.is_none()
        {
            crate::log_emotes!(
                warn,
                "refresh_from_backend_reactive: No messages in CHAT_MESSAGES for '{}'",
                channel_lower
            );
            return false;
        }

        let snapshot_changed = self
            .snapshot
            .replace_snapshot_from_backend(backend_messages);
        if let Some(generation) = blocked_reconcile_generation {
            crate::backend::blocked_users::acknowledge_chat_state_reconciled(
                &channel_lower,
                generation,
            );
        }
        if !snapshot_changed {
            let new_channel_emote_version =
                crate::state::VersionRegistry::channel_emote_version(&self.channel);
            crate::log_emotes!(
                debug,
                "refresh_from_backend_reactive: channel='{}' unchanged, skipping notify (messages={}, arc={:p}, channel_emote_version={} -> {})",
                self.channel,
                old_message_count,
                old_messages_ptr,
                old_channel_emote_version,
                new_channel_emote_version
            );
            crate::backend::metrics::record_emote_stage(
                "chat_state.refresh_from_backend_noop",
                refresh_started.elapsed(),
                self.messages.len(),
            );
            return false;
        }

        let new_channel_emote_version =
            crate::state::VersionRegistry::channel_emote_version(&self.channel);
        let new_messages_ptr = Arc::as_ptr(&self.messages);
        crate::log_emotes!(
            debug,
            "refresh_from_backend_reactive: channel='{}', messages={} -> {}, arc={:p}->{:p}, channel_emote_version={} -> {}",
            self.channel,
            old_message_count,
            self.messages.len(),
            old_messages_ptr,
            new_messages_ptr,
            old_channel_emote_version,
            new_channel_emote_version
        );

        crate::backend::metrics::record_emote_stage(
            "chat_state.refresh_from_backend_reactive",
            refresh_started.elapsed(),
            self.messages.len(),
        );
        crate::backend::metrics::record_ui_notify_source("chat_state.refresh_from_backend");
        cx.emit(ChatStateEvent::Retokenized);
        cx.notify();
        true
    }

    pub(crate) fn replace_snapshot_from_backend(&mut self, messages: Vec<ChatMessage>) -> bool {
        self.snapshot.replace_snapshot_from_backend(messages)
    }

    pub(crate) fn set_message_capacity(&mut self, target_capacity: usize) -> bool {
        self.snapshot.set_message_capacity(target_capacity)
    }

    pub(crate) fn user_decorations_changed(
        &mut self,
        user_login: Arc<str>,
        user_id: Option<Arc<str>>,
        cx: &mut Context<Self>,
    ) {
        cx.emit(ChatStateEvent::UserDecorationsChanged {
            user_login,
            user_id,
        });
    }

    /// Badge URLs resolve while shaping, so drop the badge render caches and
    /// record one channel eviction for every split of this channel to replay.
    pub(crate) fn badge_decorations_changed(&mut self, cx: &mut Context<Self>) {
        crate::components::chat::canvas_renderer::text_shaping::clear_badge_render_caches();
        if !self.messages.is_empty() {
            crate::components::chat::canvas_renderer::global_layout_cache::clear_layouts_for_channel(
                &self.channel,
            );
        }
        cx.emit(ChatStateEvent::BadgeDecorationsChanged);
    }
}
