//! Split Pane - per-split state that is not the draft
//!
//! Each SplitPane is an Entity that owns one split's:
//! - Focus handle
//! - Reply state
//! - Overlay state (emote picker, autocomplete)
//! - Pinned-chat dismissal and expansion
//!
//! The draft text, caret and history live in `InputManager`, and scrolling in
//! the split's chat view.

use gpui::*;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use crate::state::ReplyState;
pub use echo_core::SplitId;
pub use echo_core::split_overlay::{SplitAutocompleteState, SplitEmotePickerState};

/// Events emitted by SplitPane
#[derive(Clone, Debug)]
pub enum SplitPaneEvent {
    /// Channel changed (user selected different channel)
    ChannelChanged { new_channel: String },
}

/// A split-local, in-memory identity for one Twitch pinned message, used for
/// both the dismissed and the expanded pin.
///
/// Twitch can update a pin without assigning it a new message ID, so the
/// timestamp and a content hash distinguish a changed pin from the exact
/// message the user dismissed. This deliberately never enters persisted
/// workspace state.
#[derive(Clone, Debug, PartialEq, Eq)]
struct PinnedChatIdentity {
    message_id: String,
    updated_at: Option<String>,
    content_hash: u64,
}

impl PinnedChatIdentity {
    fn from_message(message_id: &str, updated_at: Option<&str>, text: &str) -> Self {
        let mut hasher = DefaultHasher::new();
        text.hash(&mut hasher);
        Self {
            message_id: message_id.to_string(),
            updated_at: updated_at.map(str::to_string),
            content_hash: hasher.finish(),
        }
    }
}

/// One split's pane state, kept apart from every other split's.
///
/// Each SplitPane is a GPUI Entity that owns:
/// - Its unique identity (SplitId)
/// - The channel it's displaying
/// - Its focus handle
/// - Reply state (which message we're replying to)
/// - Overlay state (emote picker, autocomplete)
/// - Pinned-chat dismissal and expansion
pub struct SplitPane {
    /// Stable unique identifier (survives layout changes)
    pub id: SplitId,

    /// Channel currently displayed in this split
    pub channel: String,

    /// Focus handle for this split's container
    pub focus_handle: FocusHandle,

    /// Reply state - which message we're replying to
    pub reply_to: Option<ReplyState>,

    /// Per-split emote picker state
    pub emote_picker: SplitEmotePickerState,

    /// Per-split autocomplete state
    pub autocomplete: SplitAutocompleteState,

    /// The pin dismissed in this split only. It is intentionally retained with
    /// the split entity rather than saved into workspace configuration.
    dismissed_pinned_chat: Option<PinnedChatIdentity>,

    /// Monotonic split-local key for retained leaf invalidation when the pin
    /// dismissal changes without any other visible split parameter changing.
    pinned_chat_dismissal_version: u64,

    /// The exact pin expanded by the user in this split. Like dismissal, this
    /// is deliberately in-memory only and must never follow a replacement pin.
    expanded_pinned_chat: Option<PinnedChatIdentity>,

    /// Monotonic split-local key for retained leaf invalidation when the pin
    /// bubble's expanded/collapsed state changes.
    pinned_chat_expansion_version: u64,
}

impl EventEmitter<SplitPaneEvent> for SplitPane {}

impl SplitPane {
    /// Create with a specific SplitId (for deserialization)
    pub fn with_id(id: SplitId, channel: String, cx: &mut Context<Self>) -> Self {
        let focus_handle = cx.focus_handle();

        Self {
            id,
            channel,
            focus_handle,
            reply_to: None,
            emote_picker: SplitEmotePickerState::default(),
            autocomplete: SplitAutocompleteState::default(),
            dismissed_pinned_chat: None,
            pinned_chat_dismissal_version: 0,
            expanded_pinned_chat: None,
            pinned_chat_expansion_version: 0,
        }
    }

    /// Change the channel displayed in this split
    pub fn set_channel(&mut self, channel: String, cx: &mut Context<Self>) {
        if self.channel != channel {
            let old_channel = std::mem::replace(&mut self.channel, channel.clone());

            self.reply_to = None;
            self.dismissed_pinned_chat = None;
            self.clear_pinned_chat_expansion();

            self.emote_picker.close();
            self.autocomplete.close();

            cx.emit(SplitPaneEvent::ChannelChanged {
                new_channel: channel,
            });
            cx.notify();

            crate::log_ui!(
                debug,
                "SplitPane {} changed channel: {} -> {}",
                self.id,
                old_channel,
                self.channel
            );
        }
    }

    /// Clear the reply state
    pub fn clear_reply(&mut self) {
        self.reply_to = None;
    }

    pub fn is_pinned_chat_dismissed(
        &self,
        message_id: &str,
        updated_at: Option<&str>,
        text: &str,
    ) -> bool {
        self.dismissed_pinned_chat
            .as_ref()
            .is_some_and(|dismissed| {
                *dismissed == PinnedChatIdentity::from_message(message_id, updated_at, text)
            })
    }

    #[inline]
    pub fn pinned_chat_dismissal_version(&self) -> u64 {
        self.pinned_chat_dismissal_version
    }

    #[inline]
    pub fn pinned_chat_expansion_version(&self) -> u64 {
        self.pinned_chat_expansion_version
    }

    pub fn is_pinned_chat_expanded(
        &self,
        message_id: &str,
        updated_at: Option<&str>,
        text: &str,
    ) -> bool {
        self.expanded_pinned_chat.as_ref().is_some_and(|expanded| {
            *expanded == PinnedChatIdentity::from_message(message_id, updated_at, text)
        })
    }

    /// Keep expansion scoped to the current exact pin. Twitch can replace or
    /// update a pin without changing its message ID, so a stale expansion must
    /// collapse before a replacement becomes visible.
    pub fn sync_pinned_chat_expansion(
        &mut self,
        message_id: &str,
        updated_at: Option<&str>,
        text: &str,
        cx: &mut Context<Self>,
    ) {
        if self.expanded_pinned_chat.as_ref().is_some_and(|expanded| {
            *expanded != PinnedChatIdentity::from_message(message_id, updated_at, text)
        }) && self.clear_pinned_chat_expansion()
        {
            cx.notify();
        }
    }

    pub fn toggle_pinned_chat_expansion(
        &mut self,
        message_id: &str,
        updated_at: Option<&str>,
        text: &str,
        cx: &mut Context<Self>,
    ) {
        let identity = PinnedChatIdentity::from_message(message_id, updated_at, text);
        if self.expanded_pinned_chat.as_ref() == Some(&identity) {
            self.expanded_pinned_chat = None;
        } else {
            self.expanded_pinned_chat = Some(identity);
        }
        self.pinned_chat_expansion_version = self.pinned_chat_expansion_version.wrapping_add(1);
        cx.notify();
    }

    pub fn dismiss_pinned_chat(
        &mut self,
        message_id: &str,
        updated_at: Option<&str>,
        text: &str,
        cx: &mut Context<Self>,
    ) {
        let dismissed = PinnedChatIdentity::from_message(message_id, updated_at, text);
        if self.dismissed_pinned_chat != Some(dismissed.clone()) {
            self.dismissed_pinned_chat = Some(dismissed);
            self.pinned_chat_dismissal_version = self.pinned_chat_dismissal_version.wrapping_add(1);
            cx.notify();
        }
    }

    /// Forget the dismissed identity when Twitch reports no active pin. A
    /// later re-pin of the same message should be visible again.
    pub fn clear_dismissed_pinned_chat(&mut self, cx: &mut Context<Self>) {
        if self.dismissed_pinned_chat.take().is_some() {
            self.pinned_chat_dismissal_version = self.pinned_chat_dismissal_version.wrapping_add(1);
            cx.notify();
        }
    }

    /// Forget expansion when Twitch reports no pin, or when a channel change
    /// invalidates the split-local bubble. Dismissal remains independent.
    pub fn clear_pinned_chat_expansion(&mut self) -> bool {
        if self.expanded_pinned_chat.take().is_some() {
            self.pinned_chat_expansion_version = self.pinned_chat_expansion_version.wrapping_add(1);
            true
        } else {
            false
        }
    }
}

impl Render for SplitPane {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div().id(("split-pane", self.id.as_u64()))
    }
}
