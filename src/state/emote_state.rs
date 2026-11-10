//! Emote State Manager - GPUI Entity for reactive emote state
//!
//! This module provides a reactive pattern for emote state changes using GPUI's
//! Entity system. Components can observe this entity to automatically re-render
//! when emotes are loaded or updated.
//!
//! Key features:
//! - Global emote version tracking
//! - Per-channel emote version tracking
//! - Event emission for emote state changes
//! - Integration with the emote loading pipeline

use gpui::*;
use std::collections::HashMap;

/// Events emitted when emote state changes
#[derive(Clone, Debug)]
pub enum EmoteStateEvent {
    /// Global emotes have been loaded/updated
    GlobalEmotesLoaded { emote_count: usize },

    /// User-specific emotes have been loaded/updated
    UserEmotesLoaded { emote_count: usize },

    /// Channel-specific emotes have been loaded/updated
    ChannelEmotesLoaded { channel: String, emote_count: usize },
}

/// GPUI Entity for managing emote state and notifying observers of changes.
///
/// This provides a reactive pattern for emote loading:
/// 1. Backend loads emotes asynchronously
/// 2. EmoteStateManager is notified of the change
/// 3. Observers (split panes, canvas renderers) automatically re-render
///
/// This eliminates the race condition where splits might see different
/// emote states depending on when they join a channel.
pub struct EmoteStateManager {
    /// Global emote version counter
    global_version: u64,

    /// User-specific emote version counter
    user_version: u64,

    /// Per-channel emote version counters
    channel_versions: HashMap<String, u64>,

    /// Channels that are currently loading emotes
    loading_channels: Vec<String>,

    /// Whether global emotes are currently loading
    global_loading: bool,
}

impl EventEmitter<EmoteStateEvent> for EmoteStateManager {}

impl EmoteStateManager {
    /// Create a new EmoteStateManager
    pub fn new() -> Self {
        Self {
            global_version: 0,
            user_version: 0,
            channel_versions: HashMap::new(),
            loading_channels: Vec::new(),
            global_loading: false,
        }
    }

    /// Notify that global emotes have been loaded
    pub fn notify_global_emotes_loaded(&mut self, emote_count: usize, cx: &mut Context<Self>) {
        self.global_version += 1;
        self.global_loading = false;

        crate::log_emotes!(
            info,
            "Global emotes loaded: {} emotes, version now {}",
            emote_count,
            self.global_version
        );

        crate::state::ui_invalidation::request(
            crate::state::ui_invalidation::UiInvalidation::ProviderMetadata,
        );
        cx.emit(EmoteStateEvent::GlobalEmotesLoaded { emote_count });
        cx.notify();
    }

    /// Notify that user-specific emotes have been loaded
    pub fn notify_user_emotes_loaded(&mut self, emote_count: usize, cx: &mut Context<Self>) {
        self.user_version += 1;

        crate::log_emotes!(
            info,
            "User emotes loaded: {} emotes, version now {}",
            emote_count,
            self.user_version
        );

        crate::state::ui_invalidation::request(
            crate::state::ui_invalidation::UiInvalidation::ProviderMetadata,
        );
        cx.emit(EmoteStateEvent::UserEmotesLoaded { emote_count });
        cx.notify();
    }

    /// Notify that channel emotes have been loaded
    pub fn notify_channel_emotes_loaded(
        &mut self,
        channel: &str,
        emote_count: usize,
        cx: &mut Context<Self>,
    ) {
        let channel_lower = channel.to_lowercase();

        let version = self
            .channel_versions
            .entry(channel_lower.clone())
            .or_insert(0);
        *version += 1;

        self.loading_channels.retain(|c| c != &channel_lower);

        crate::log_emotes!(
            info,
            "Channel {} emotes loaded: {} emotes, version now {}",
            channel,
            emote_count,
            version
        );

        crate::state::ui_invalidation::request(
            crate::state::ui_invalidation::UiInvalidation::ProviderMetadata,
        );
        cx.emit(EmoteStateEvent::ChannelEmotesLoaded {
            channel: channel_lower,
            emote_count,
        });
        cx.notify();
    }
}

impl Default for EmoteStateManager {
    fn default() -> Self {
        Self::new()
    }
}
