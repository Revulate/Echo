//! User Card overlay with mod actions
//!
//! Draggable card showing:
//! - User avatar, username, badges
//! - Action buttons (Mention, Whisper, Profile)
//! - Recent messages from user
//! - Configurable moderation buttons
//! - Ban/Unban buttons
//!
//! Opened via username click in chat messages

use super::{ConfirmationModal, ConfirmationState, text_tooltip_on_hover, traits::Openable};
use crate::backend;
use crate::backend::cosmetics_core::{ResolvedUserCosmetics, SevenTvBadge};
use crate::backend::irc::{get_user_color, resolve_user_color};
use crate::backend::slots::{ResolvedSlot, SlotResolver};
use crate::cache::loader as image_cache;
use crate::components::ui::ButtonExt;
use crate::components::ui::input::{
    Escape, Input, InputEvent, InputState, apply_standard_input_chrome,
};
use crate::theme::tokens::Spacing;
use crate::theme::{
    colors,
    tokens::{Radii, RadiiPixels, Sizes},
};
use crate::utils::{
    copy_text, format_message_timestamp, parse_hex_color_opt, parse_user_color_opt, truncate_text,
};
use gpui::prelude::FluentBuilder;
use gpui::*;
use std::sync::Arc;
use std::time::Duration;

const USER_CARD_METADATA_FALLBACK_INTERVAL_MS: u64 = 1_000;
const USER_CARD_METADATA_FALLBACK_TICKS: usize = 12;
const USER_CARD_NOTE_MAX_LEN: usize = 180;
/// Idle time after the last keystroke before the local note auto-saves.
const NOTE_AUTOSAVE_DEBOUNCE_MS: u64 = 800;

#[derive(Clone)]
struct TwitchBlockAction {
    user: backend::blocked_users::BlockedUser,
    blocked: bool,
}

struct PendingBanConfirmation {
    modal: Entity<ConfirmationModal>,
    active: Arc<std::sync::atomic::AtomicBool>,
    _subscription: Subscription,
}

impl Drop for PendingBanConfirmation {
    fn drop(&mut self) {
        self.active
            .store(false, std::sync::atomic::Ordering::Release);
    }
}

struct UserCardObserver {
    username: String,
    updates: tokio::sync::watch::Sender<()>,
}

impl backend::observers::ChannelObserver for UserCardObserver {
    fn on_event(&self, _channel: &str, event: backend::observers::ChannelEvent) {
        use backend::observers::ChannelEvent;

        let relevant = match event {
            ChannelEvent::NewMessage(message) => {
                message.sender.eq_ignore_ascii_case(&self.username)
            }
            ChannelEvent::UserDecorationsChanged { user_login, .. } => {
                user_login.eq_ignore_ascii_case(&self.username)
            }
            ChannelEvent::HistoryLoaded(_)
            | ChannelEvent::MessagesRetokenized
            | ChannelEvent::BadgeDecorationsChanged => true,
            ChannelEvent::RoomStateChanged => false,
        };
        if relevant {
            self.updates.send_replace(());
        }
    }
}

/// User card state
#[derive(Clone, Debug, Default)]
pub struct UserCardState {
    pub is_open: bool,
    pub username: Option<String>,
    pub display_name: Option<String>,
    pub user_id: Option<String>,
    pub channel: Option<String>,
    pub color: Option<String>,
    pub badges: Vec<String>,
    pub x: f32,
    pub y: f32,
}

impl Openable for UserCardState {
    fn set_open(&mut self, open: bool) {
        self.is_open = open;
    }
}

impl UserCardState {
    #[allow(clippy::too_many_arguments)]
    pub fn open(
        username: String,
        display_name: Option<String>,
        user_id: Option<String>,
        channel: String,
        color: Option<String>,
        badges: Vec<String>,
        x: f32,
        y: f32,
    ) -> Self {
        Self {
            is_open: true,
            username: Some(username),
            display_name,
            user_id,
            channel: Some(channel),
            color,
            badges,
            x,
            y,
        }
    }

    /// Close the user card and clear state
    pub fn close(&mut self) {
        Openable::close(self);
        self.username = None;
        self.display_name = None;
        self.user_id = None;
        self.channel = None;
        self.color = None;
        self.badges.clear();
    }

    pub fn known_user_id(&self) -> Option<&str> {
        self.user_id
            .as_deref()
            .map(str::trim)
            .filter(|id| !id.is_empty())
    }
}

/// Cached metadata used to suppress unchanged fallback refreshes.
#[derive(Clone, Copy, PartialEq)]
struct MetadataSnapshot {
    context_fingerprint: u64,
    profile_state: backend::twitch_api::UserCardFetchState,
    follow_state: backend::twitch_api::UserCardFetchState,
    subscription_state: backend::twitch_api::UserCardFetchState,
    profile_present: bool,
    follow_present: bool,
    subscription_present: bool,
    cosmetics_present: bool,
    pronouns_present: bool,
    /// A queued 7TV cosmetics fetch that has not produced a cache entry yet.
    cosmetics_pending: bool,
    /// A queued pronoun fetch that has not produced a cache entry yet.
    pronouns_pending: bool,
    live_status: Option<bool>,
    live_status_pending: bool,
    external_badge_count: usize,
    likely_banned: bool,
    block_state: Option<bool>,
}

impl Default for MetadataSnapshot {
    fn default() -> Self {
        Self {
            context_fingerprint: 0,
            profile_state: backend::twitch_api::UserCardFetchState::Idle,
            follow_state: backend::twitch_api::UserCardFetchState::Idle,
            subscription_state: backend::twitch_api::UserCardFetchState::Idle,
            profile_present: false,
            follow_present: false,
            subscription_present: false,
            cosmetics_present: false,
            pronouns_present: false,
            cosmetics_pending: false,
            pronouns_pending: false,
            live_status: None,
            live_status_pending: false,
            external_badge_count: 0,
            likely_banned: false,
            block_state: None,
        }
    }
}

/// Identity of the inputs behind `UserCard::active_cosmetic_animation_urls`.
#[derive(Clone, PartialEq)]
struct AnimationUrlsKey {
    rows_generation: u64,
    avatar_url: Option<String>,
    badge_url: Option<String>,
}

/// Cache identity for the derived recent-message rows.
#[derive(Clone, Copy, PartialEq)]
struct RecentRowsKey {
    context_generation: u64,
    emote_resolution_version: u64,
    channel_emote_version: u64,
    global_emote_version: u64,
    user_emote_version: u64,
    layout_settings_version: u64,
    /// Rows bake resolved sender and @mention colors, so a color learned after
    /// the rows were built has to invalidate them.
    user_color_generation: u64,
    show_timestamps: bool,
    timestamp_format: backend::settings::MessageTimestampFormat,
    hide_user_details: bool,
    window_scale: f32,
}

/// User card overlay component
pub struct UserCard {
    pub state: UserCardState,
    user_context: backend::UserMessageContext,
    /// Fingerprint of the buffered messages behind `user_context`; refresh ticks
    /// matching it skip the reload entirely.
    user_context_fingerprint: u64,
    /// Bumped whenever `user_context` is replaced; keys the row cache.
    user_context_generation: u64,
    metadata_snapshot: MetadataSnapshot,
    /// Derived render inputs for `user_context.recent_messages`, so animation
    /// ticks reuse resolved slots and shared strings.
    recent_rows: Vec<RecentMessageRow>,
    recent_rows_key: Option<RecentRowsKey>,
    /// Bumped on every row rebuild.
    recent_rows_generation: u64,
    /// Animated emote URLs contributed by `recent_rows`, in first-seen order.
    recent_row_animation_urls: Vec<String>,
    animation_urls_key: Option<AnimationUrlsKey>,
    active_cosmetic_animation_urls: Vec<String>,
    cosmetic_animation_waker_running: bool,
    metadata_refresh_generation: u64,
    metadata_refresh_task: Option<Task<()>>,
    header_meta_reserved_lines: Option<usize>,
    ban_confirmation: Option<PendingBanConfirmation>,
    note_input: Option<Entity<InputState>>,
    note_input_bound_user: Option<String>,
    /// Reason field for Warn; Twitch shows the reason to the warned user, so
    /// the card never sends a canned one.
    warn_input: Option<Entity<InputState>>,
    warn_prompt_open: bool,
    note_status_message: Option<String>,
    note_status_is_error: bool,
    /// Unsaved note edits pending the auto-save debounce.
    note_dirty: bool,
    note_last_edit: Option<std::time::Instant>,
    note_autosave_waker_running: bool,
    /// Focus target used to finish note editing without closing the card.
    note_dismiss_focus: Option<FocusHandle>,
    subscriptions: Vec<Subscription>,
    /// Drag state
    is_dragging: bool,
    /// Drag start offset (relative to card origin)
    drag_offset: Point<Pixels>,
    /// When true the card is the root of its own OS window (popout): it fills
    /// a borderless, edge-resizable window and its header becomes the move
    /// surface, so overlay positioning and card-local dragging are disabled.
    pub hosted_in_window: bool,
    /// Focus target that takes over when an inline prompt closes.
    pub host_focus: Option<FocusHandle>,
}

impl UserCard {
    pub fn new(state: UserCardState) -> Self {
        let user_context = Self::load_user_context(&state);
        let user_context_fingerprint = Self::user_context_fingerprint(&state);
        Self::queue_user_card_cosmetics(&state, &user_context);
        Self::queue_user_card_pronouns(&state);
        Self::queue_user_card_twitch_profile(&state);
        Self::queue_user_card_live_status(&state);
        Self::queue_user_card_follow(&state, &user_context);
        Self::queue_user_card_subscription(&state, &user_context);

        let mut card = Self {
            state,
            user_context,
            user_context_fingerprint,
            user_context_generation: 0,
            metadata_snapshot: MetadataSnapshot::default(),
            recent_rows: Vec::new(),
            recent_rows_key: None,
            recent_rows_generation: 0,
            recent_row_animation_urls: Vec::new(),
            animation_urls_key: None,
            active_cosmetic_animation_urls: Vec::new(),
            cosmetic_animation_waker_running: false,
            metadata_refresh_generation: 0,
            metadata_refresh_task: None,
            header_meta_reserved_lines: None,
            ban_confirmation: None,
            note_input: None,
            note_input_bound_user: None,
            warn_input: None,
            warn_prompt_open: false,
            note_status_message: None,
            note_status_is_error: false,
            note_dirty: false,
            note_last_edit: None,
            note_autosave_waker_running: false,
            note_dismiss_focus: None,
            subscriptions: Vec::new(),
            is_dragging: false,
            drag_offset: Point::default(),
            hosted_in_window: false,
            host_focus: None,
        };
        card.metadata_snapshot = card.capture_metadata_snapshot();
        card
    }

    pub fn update_state(&mut self, state: UserCardState) {
        self.header_meta_reserved_lines = None;
        self.ban_confirmation = None;
        self.metadata_refresh_task = None;
        self.metadata_refresh_generation = self.metadata_refresh_generation.wrapping_add(1);
        self.state = state;
        self.user_context = Self::load_user_context(&self.state);
        self.user_context_fingerprint = Self::user_context_fingerprint(&self.state);
        self.user_context_generation = self.user_context_generation.wrapping_add(1);
        Self::queue_user_card_cosmetics(&self.state, &self.user_context);
        Self::queue_user_card_pronouns(&self.state);
        Self::queue_user_card_twitch_profile(&self.state);
        Self::queue_user_card_live_status(&self.state);
        Self::queue_user_card_follow(&self.state, &self.user_context);
        Self::queue_user_card_subscription(&self.state, &self.user_context);
        self.metadata_snapshot = self.capture_metadata_snapshot();
        self.note_input_bound_user = None;
        self.note_status_message = None;
        self.note_status_is_error = false;
        self.note_dirty = false;
        self.note_last_edit = None;
        self.warn_prompt_open = false;
    }

    fn open_ban_confirmation(
        &mut self,
        channel: &str,
        username: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.state.is_open
            || !self.can_moderate_current_target()
            || !self
                .state
                .channel
                .as_deref()
                .is_some_and(|current| current.eq_ignore_ascii_case(channel))
            || !self
                .state
                .username
                .as_deref()
                .is_some_and(|current| current.eq_ignore_ascii_case(username))
            || !crate::components::common::moderation_policy::recheck_moderation_target(
                channel, username,
            )
        {
            return;
        }
        let channel = channel.to_string();
        let username = username.to_string();
        let auth_generation = backend::auth::credential_generation();
        let active = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let confirm_active = Arc::clone(&active);
        let cancel_active = Arc::clone(&active);
        let modal = cx.new(ConfirmationModal::new);
        let focus = modal.read(cx).focus_handle.clone();
        modal.update(cx, |modal, cx| {
            modal.state = ConfirmationState {
                is_open: true,
                title: "Ban user?".to_string(),
                message: format!("Ban @{username} in #{}?", channel.trim_start_matches('#')),
                confirm_text: "Ban".to_string(),
                cancel_text: "Cancel".to_string(),
                is_destructive: true,
                confirm_focused: false,
                ..Default::default()
            };
            modal.on_confirm = Some(Arc::new(move || {
                if confirm_active.swap(false, std::sync::atomic::Ordering::AcqRel)
                    && backend::auth::credential_generation() == auth_generation
                    && crate::components::common::moderation_policy::recheck_moderation_target(
                        &channel, &username,
                    )
                {
                    backend::ban_user(&channel, &username, None);
                }
            }));
            modal.on_cancel = Some(Arc::new(move || {
                cancel_active.store(false, std::sync::atomic::Ordering::Release);
            }));
            cx.notify();
        });
        let previous_focus = window.focused(cx);
        let host = window.window_handle();
        let subscription = cx.observe(&modal, move |_, modal, cx| {
            if !modal.read(cx).state.is_open {
                let previous_focus = previous_focus.clone();
                cx.defer(move |cx| {
                    let _ = host.update(cx, |_, window, cx| {
                        if let Some(focus) = previous_focus {
                            window.focus(&focus, cx);
                        }
                    });
                });
            }
            cx.notify();
        });
        self.ban_confirmation = Some(PendingBanConfirmation {
            modal,
            active,
            _subscription: subscription,
        });
        window.focus(&focus, cx);
        cx.notify();
    }

    fn open_warn_prompt(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let input = match self.warn_input.clone() {
            Some(input) => input,
            None => {
                let input = cx.new(|cx| {
                    InputState::new(window, cx).with_placeholder("Reason shown to the user…")
                });
                let subscription = cx.subscribe(&input, |this, _input, event: &InputEvent, cx| {
                    if matches!(event, InputEvent::PressEnter) {
                        this.submit_warn(cx);
                    }
                });
                self.subscriptions.push(subscription);
                self.warn_input = Some(input.clone());
                input
            }
        };
        input.update(cx, |state, cx| state.set_value(String::new(), window, cx));
        let focus_handle = input.read(cx).focus_handle().clone();
        window.focus(&focus_handle, cx);
        self.warn_prompt_open = true;
        cx.notify();
    }

    fn submit_warn(&mut self, cx: &mut Context<Self>) {
        let Some(input) = self.warn_input.clone() else {
            return;
        };
        let reason = input.read(cx).value().trim().to_string();
        if reason.is_empty() {
            return;
        }
        if let (Some(channel), Some(username)) =
            (self.state.channel.clone(), self.state.username.clone())
            && crate::components::common::moderation_policy::recheck_moderation_target(
                &channel, &username,
            )
        {
            backend::send_message_or_command_async(channel, format!("/warn {username} {reason}"));
        }
        self.warn_prompt_open = false;
        cx.notify();
    }

    fn load_user_context(state: &UserCardState) -> backend::UserMessageContext {
        if let (Some(channel), Some(username)) = (&state.channel, &state.username) {
            backend::get_user_message_context(channel, username, 250)
        } else {
            backend::UserMessageContext::default()
        }
    }

    fn user_context_fingerprint(state: &UserCardState) -> u64 {
        if let (Some(channel), Some(username)) = (&state.channel, &state.username) {
            backend::bridge::get_user_message_context_fingerprint(channel, username)
        } else {
            0
        }
    }

    /// Read every asynchronous input the refresh loop is waiting on. Kept to
    /// cache lookups so a tick that changes nothing stays cheap.
    fn capture_metadata_snapshot(&self) -> MetadataSnapshot {
        let username = self.state.username.clone().unwrap_or_default();
        let channel = self.state.channel.clone().unwrap_or_default();
        let twitch_profile = backend::twitch_api::get_cached_user_profile(&username);
        let user_id = self.resolved_user_id(twitch_profile.as_ref());
        let user_id = user_id.as_deref();

        let cosmetics_present = user_id
            .and_then(backend::cosmetics::get_cached_user_cosmetics)
            .is_some();
        let pronouns_present = backend::pronouns::get_pronouns(&username).is_some();
        let cosmetics_queued = !channel.is_empty()
            && !username.is_empty()
            && self
                .state
                .known_user_id()
                .or(self.user_context.latest_user_id.as_deref())
                .is_some_and(|id| !id.trim().is_empty());
        let pronouns_queued = backend::settings::get_show_pronouns() && !username.is_empty();

        MetadataSnapshot {
            context_fingerprint: self.user_context_fingerprint,
            profile_state: backend::twitch_api::get_user_profile_status(&username).state,
            follow_state: backend::twitch_api::get_user_follow_status(&channel, &username, user_id)
                .state,
            subscription_state: backend::twitch_api::get_user_subscription_status(
                &channel, &username, user_id,
            )
            .state,
            profile_present: twitch_profile.is_some(),
            follow_present: backend::twitch_api::get_cached_user_follow(
                &channel, &username, user_id,
            )
            .is_some(),
            subscription_present: backend::twitch_api::get_cached_user_subscription(
                &channel, &username, user_id,
            )
            .is_some(),
            cosmetics_present,
            pronouns_present,
            cosmetics_pending: cosmetics_queued && !cosmetics_present,
            pronouns_pending: pronouns_queued && !pronouns_present,
            live_status: backend::twitch_api::get_cached_user_live_status(&username),
            live_status_pending: backend::twitch_api::is_user_live_status_pending(&username),
            external_badge_count: backend::get_external_badge_keys(&channel, &username, user_id)
                .len(),
            likely_banned: crate::backend::components::mod_log::is_user_likely_banned(
                &channel, &username,
            ),
            block_state: user_id
                .and_then(|id| backend::blocked_users::block_action_state(id, &username)),
        }
    }

    /// Rebuild the derived recent-message rows when the context, emote
    /// resolution, or the settings baked into them have moved. Slot resolution
    /// and string building happen here so a render only assembles elements.
    fn ensure_recent_rows(&mut self, channel: &str, hide_user_details: bool, window_scale: f32) {
        let show_timestamps = backend::settings::should_show_message_timestamps(
            backend::bridge::get_cached_channel_live_status(channel),
        );
        let key = RecentRowsKey {
            context_generation: self.user_context_generation,
            emote_resolution_version: crate::state::VersionRegistry::emote_resolution_version(),
            channel_emote_version: crate::state::VersionRegistry::channel_emote_version(channel),
            global_emote_version: crate::state::VersionRegistry::global_emote_version(),
            user_emote_version: crate::state::VersionRegistry::user_emote_version(),
            layout_settings_version: backend::settings::get_layout_settings_version(),
            user_color_generation: backend::irc::user_color_generation(),
            show_timestamps,
            timestamp_format: backend::settings::get_message_timestamp_format(),
            hide_user_details,
            window_scale,
        };
        if self.recent_rows_key == Some(key) {
            return;
        }
        self.recent_rows_key = Some(key);
        self.recent_rows_generation = self.recent_rows_generation.wrapping_add(1);

        if hide_user_details {
            self.recent_rows = Vec::new();
            self.recent_row_animation_urls = Vec::new();
            return;
        }

        let resolver = SlotResolver::new(channel);
        let display_rules = backend::settings::emote_display_rules_snapshot();
        let mut rows = Vec::with_capacity(self.user_context.recent_messages.len());
        let mut animation_urls: Vec<String> = Vec::new();
        let mut seen_animation_urls: rustc_hash::FxHashSet<String> = Default::default();

        for msg in &self.user_context.recent_messages {
            let mut parts = Vec::with_capacity(msg.slots.len());
            for slot in msg.slots.iter() {
                parts.push(match resolver.resolve(slot) {
                    ResolvedSlot::Text(text) => RecentMessagePart::Text(SharedString::from(text)),
                    ResolvedSlot::TwitchGif { alt_text, .. } => {
                        RecentMessagePart::Text(SharedString::from(alt_text))
                    }
                    ResolvedSlot::Emote(emote) => {
                        let flags = display_rules
                            .as_deref()
                            .map(|rules| rules.flags_for(emote.provider, &emote.id, &msg.channel))
                            .unwrap_or_default();
                        if flags.show_as_text {
                            // What was typed, shown as message text.
                            let code = slot.emote_source_text().unwrap_or(&emote.name);
                            RecentMessagePart::Text(SharedString::from(code.to_string()))
                        } else {
                            let url = backend::emotes::chat_display_url_shared_for_scale(
                                &emote,
                                window_scale,
                            )
                            .to_string();
                            if emote.is_animated
                                && !flags.still_image
                                && animation_urls.len() < 64
                                && !seen_animation_urls.contains(url.as_str())
                            {
                                seen_animation_urls.insert(url.clone());
                                animation_urls.push(url.clone());
                            }
                            let image_id =
                                SharedString::from(format!("user-card-inline:{}", emote.name));
                            RecentMessagePart::Emote {
                                url: SharedString::from(url),
                                image_id,
                                emote,
                                still: flags.still_image,
                            }
                        }
                    }
                    ResolvedSlot::Url { url, .. } => {
                        RecentMessagePart::Url(SharedString::from(url))
                    }
                    ResolvedSlot::Mention(mention) => RecentMessagePart::Mention {
                        text: SharedString::from(format!("@{}", mention)),
                        color: get_user_color(&mention)
                            .as_deref()
                            .and_then(parse_user_color_opt),
                    },
                    ResolvedSlot::Emoji { emoji, url } => RecentMessagePart::Emoji {
                        image_id: SharedString::from(format!("user-card-inline:{}", emoji)),
                        url: SharedString::from(url),
                    },
                });
            }

            rows.push(RecentMessageRow {
                timestamp: show_timestamps
                    .then(|| format_message_timestamp(msg.timestamp))
                    .flatten()
                    .map(SharedString::from),
                parts,
            });
        }

        self.recent_rows = rows;
        self.recent_row_animation_urls = animation_urls;
    }

    fn queue_user_card_cosmetics(
        state: &UserCardState,
        user_context: &backend::UserMessageContext,
    ) {
        let (Some(channel), Some(username)) = (state.channel.as_ref(), state.username.as_ref())
        else {
            return;
        };
        let Some(user_id) = state
            .known_user_id()
            .or(user_context.latest_user_id.as_deref())
        else {
            return;
        };

        backend::cosmetics::queue_visible_cosmetics_batch(vec![(
            Arc::from(user_id),
            Arc::from(username.as_str()),
            Arc::from(channel.as_str()),
        )]);
    }

    fn queue_user_card_pronouns(state: &UserCardState) {
        if !backend::settings::get_show_pronouns() {
            return;
        }
        if let Some(username) = state.username.as_ref() {
            backend::pronouns::queue_fetch_pronouns(username);
        }
    }

    fn queue_user_card_twitch_profile(state: &UserCardState) {
        if backend::streamer_mode::should_hide_user_card_details() {
            return;
        }
        if let Some(username) = state.username.as_ref() {
            backend::twitch_api::queue_fetch_user_profile(username);
        }
    }

    fn queue_user_card_live_status(state: &UserCardState) {
        if backend::streamer_mode::should_hide_user_card_details() {
            return;
        }
        if let Some(username) = state.username.as_ref() {
            backend::twitch_api::queue_user_live_status(username);
        }
    }

    fn queue_user_card_follow(state: &UserCardState, user_context: &backend::UserMessageContext) {
        if backend::streamer_mode::should_hide_user_card_details()
            || (!backend::settings::get_show_follow_date()
                && !backend::settings::get_show_follower_count())
        {
            return;
        }

        let (Some(channel), Some(username)) = (state.channel.as_ref(), state.username.as_ref())
        else {
            return;
        };
        if !Self::can_view_follow_metadata(channel) {
            return;
        }
        let user_id = state
            .known_user_id()
            .or(user_context.latest_user_id.as_deref());
        backend::twitch_api::queue_fetch_user_follow(channel, username, user_id);
    }

    fn queue_user_card_subscription(
        state: &UserCardState,
        user_context: &backend::UserMessageContext,
    ) {
        if backend::streamer_mode::should_hide_user_card_details() {
            return;
        }

        let (Some(channel), Some(username)) = (state.channel.as_ref(), state.username.as_ref())
        else {
            return;
        };
        if !Self::can_view_subscription_metadata(channel, username) {
            return;
        }
        let user_id = state
            .known_user_id()
            .or(user_context.latest_user_id.as_deref());
        backend::twitch_api::queue_fetch_user_subscription(channel, username, user_id);
    }

    /// Check if the user card is open
    pub fn is_open(&self) -> bool {
        self.state.is_open
    }

    /// Get the moderation target (username, channel) if card is open
    pub fn get_moderation_target(&self) -> Option<(String, String)> {
        if self.state.is_open
            && self.can_moderate_current_target()
            && let (Some(username), Some(channel)) = (&self.state.username, &self.state.channel)
        {
            return Some((username.clone(), channel.clone()));
        }
        None
    }

    fn can_view_follow_metadata(channel: &str) -> bool {
        backend::settings::get_show_follower_count()
            || (backend::settings::get_show_follow_date()
                && backend::irc::can_moderate_channel(channel))
    }

    fn can_view_subscription_metadata(channel: &str, username: &str) -> bool {
        let viewer = backend::settings::get_username_lowercase();
        !viewer.is_empty()
            && (viewer.eq_ignore_ascii_case(channel.trim_start_matches('#'))
                || viewer.eq_ignore_ascii_case(username.trim_start_matches('@')))
    }

    fn can_moderate_current_target(&self) -> bool {
        let (Some(channel), Some(username)) = (&self.state.channel, &self.state.username) else {
            return false;
        };
        let is_broadcaster_target = self
            .state
            .badges
            .iter()
            .chain(self.user_context.latest_badges.iter())
            .any(|badge| backend::badges::badge_key_matches(badge, "broadcaster"));
        crate::components::common::moderation_policy::can_moderate_target_with_broadcaster_target(
            channel,
            username,
            is_broadcaster_target,
        )
    }

    fn get_user_color(&self) -> Hsla {
        let username = self.state.username.as_deref().unwrap_or_default();
        let observed_color = self
            .state
            .color
            .as_deref()
            .or(self.user_context.latest_color.as_deref());
        if let Some(color) = resolve_user_color(
            self.state
                .known_user_id()
                .or(self.user_context.latest_user_id.as_deref()),
            username,
            observed_color,
        )
        .as_deref()
        .and_then(parse_user_color_opt)
        {
            return color;
        }
        colors::accent()
    }

    fn resolved_user_id(
        &self,
        twitch_profile: Option<&backend::twitch_api::TwitchUserProfile>,
    ) -> Option<String> {
        self.state
            .known_user_id()
            .map(ToOwned::to_owned)
            .or_else(|| self.user_context.latest_user_id.clone())
            .or_else(|| twitch_profile.map(|profile| profile.id.clone()))
            .filter(|id| !id.trim().is_empty())
    }

    /// Hash the animation frames the card would paint right now.
    ///
    /// `None` means the painted frame cannot be proven: the emote is still
    /// decoding, or its exact frame is not resident and the cache substitutes a
    /// held fallback we cannot predict from here. Callers must repaint
    /// unconditionally then, because the render pass is what drives the refill.
    fn displayed_animation_frame_signature(urls: &[String]) -> Option<u64> {
        use std::hash::{Hash, Hasher};

        let time_ms = crate::cache::frame_layer::current_animation_time_ms();
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        for url in urls {
            let metadata = image_cache::get_animation_metadata(url)?;
            let frame_idx = metadata.frame_index_at(time_ms);
            if !image_cache::has_animated_frame(url, frame_idx) {
                return None;
            }
            url.hash(&mut hasher);
            frame_idx.hash(&mut hasher);
        }
        Some(hasher.finish())
    }

    fn update_cosmetic_animation_waker(&mut self, urls: Vec<String>, cx: &mut Context<Self>) {
        self.active_cosmetic_animation_urls = urls;
        if !crate::emote_renderer::animation_enabled() {
            self.active_cosmetic_animation_urls.clear();
            self.cosmetic_animation_waker_running = false;
            return;
        }
        if self.active_cosmetic_animation_urls.is_empty() || self.cosmetic_animation_waker_running {
            return;
        }

        self.cosmetic_animation_waker_running = true;
        cx.spawn(async move |this, cx| {
            let mut last_frame_signature: Option<u64> = None;
            loop {
                let min_tick =
                    crate::managers::dynamic_tick_scheduler::animation_min_tick_interval_ms();
                let interval_ms = crate::emote_renderer::current_animation_interval_ms()
                    .max(min_tick)
                    .max(16);

                cx.background_executor()
                    .timer(Duration::from_millis(interval_ms))
                    .await;

                let keep_running = this
                    .update(cx, |card, cx| {
                        if !crate::emote_renderer::animation_enabled() {
                            card.active_cosmetic_animation_urls.clear();
                            card.cosmetic_animation_waker_running = false;
                            return false;
                        }
                        if !card.state.is_open {
                            card.active_cosmetic_animation_urls.clear();
                        }
                        card.active_cosmetic_animation_urls.retain(|url| {
                            !matches!(
                                image_cache::get_animated_cache_state(url),
                                image_cache::AnimatedCacheState::Failed
                            )
                        });
                        if card.active_cosmetic_animation_urls.is_empty() {
                            card.cosmetic_animation_waker_running = false;
                            return false;
                        }

                        match Self::displayed_animation_frame_signature(
                            &card.active_cosmetic_animation_urls,
                        ) {
                            Some(signature) if last_frame_signature == Some(signature) => {
                                crate::backend::metrics::record_emote_event(
                                    "user_card.animation_frame_unchanged_skipped",
                                );
                                return true;
                            }
                            signature => last_frame_signature = signature,
                        }

                        crate::backend::metrics::record_ui_notify_source(
                            "user_card.cosmetic_animation_waker",
                        );
                        cx.notify();
                        true
                    })
                    .unwrap_or(false);

                if !keep_running {
                    break;
                }
            }
        })
        .detach();
    }

    fn start_metadata_refresh_waker(&mut self, cx: &mut Context<Self>) {
        if self.metadata_refresh_task.is_some() || !self.state.is_open {
            return;
        }
        let Some(username) = self.state.username.clone() else {
            return;
        };
        let channel = self.state.channel.clone().unwrap_or_default();
        let generation = self.metadata_refresh_generation;
        let mut completions = backend::twitch_api::subscribe_user_card_metadata_updates();
        let (updates, mut channel_updates) = tokio::sync::watch::channel(());
        let observer = Arc::new(UserCardObserver {
            username: username.clone(),
            updates,
        });
        backend::observers::get_observer_registry().subscribe(&channel, observer.clone());

        self.metadata_refresh_task = Some(cx.spawn(async move |this, cx| {
            let _observer = observer;
            let mut fallback_ticks = 0;
            let fallback_interval = Duration::from_millis(USER_CARD_METADATA_FALLBACK_INTERVAL_MS);
            let mut next_fallback = std::time::Instant::now() + fallback_interval;
            loop {
                let (refresh_context, force_notify) = tokio::select! {
                    completion = completions.recv() => {
                        match completion {
                            Ok(update) => {
                                if !update.username.eq_ignore_ascii_case(&username)
                                    || update.channel.as_deref().is_some_and(|target| {
                                        !target.eq_ignore_ascii_case(&channel)
                                    })
                                {
                                    continue;
                                }
                            }
                            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                            Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                        }
                        (false, true)
                    }
                    _ = channel_updates.changed() => (true, true),
                    _ = cx.background_executor().timer(
                        next_fallback.saturating_duration_since(std::time::Instant::now()),
                    ), if fallback_ticks < USER_CARD_METADATA_FALLBACK_TICKS => {
                        // Pronouns and empty cosmetics results have no completion signal.
                        fallback_ticks += 1;
                        next_fallback = std::time::Instant::now() + fallback_interval;
                        (true, false)
                    }
                };

                let keep_running = this
                    .update(cx, |card, cx| {
                        if !card.state.is_open || card.metadata_refresh_generation != generation {
                            return false;
                        }
                        if refresh_context {
                            let fingerprint = Self::user_context_fingerprint(&card.state);
                            if fingerprint != card.user_context_fingerprint {
                                card.user_context = Self::load_user_context(&card.state);
                                card.user_context_fingerprint = fingerprint;
                                card.user_context_generation =
                                    card.user_context_generation.wrapping_add(1);
                            }
                        }
                        Self::queue_user_card_cosmetics(&card.state, &card.user_context);
                        Self::queue_user_card_pronouns(&card.state);
                        Self::queue_user_card_twitch_profile(&card.state);
                        Self::queue_user_card_live_status(&card.state);
                        Self::queue_user_card_follow(&card.state, &card.user_context);
                        Self::queue_user_card_subscription(&card.state, &card.user_context);

                        let snapshot = card.capture_metadata_snapshot();
                        if snapshot != card.metadata_snapshot || force_notify {
                            card.metadata_snapshot = snapshot;
                            crate::backend::metrics::record_ui_notify_source(
                                "user_card.metadata_refresh_waker",
                            );
                            cx.notify();
                        }
                        true
                    })
                    .unwrap_or(false);
                if !keep_running {
                    break;
                }
            }
        }));
    }

    fn ensure_note_input(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
        username: &str,
        current_note: &str,
    ) {
        if self.note_dismiss_focus.is_none() {
            self.note_dismiss_focus = Some(cx.focus_handle());
        }

        if self.note_input.is_none() {
            let input =
                cx.new(|cx| InputState::new(window, cx).with_placeholder("Add local note…"));
            let subscription =
                cx.subscribe(&input, |this, input, event: &InputEvent, cx| match event {
                    InputEvent::Change => {
                        let value = input.read(cx).value();
                        if value.chars().count() > USER_CARD_NOTE_MAX_LEN {
                            this.note_status_message = Some(format!(
                                "Local notes are limited to {} characters",
                                USER_CARD_NOTE_MAX_LEN
                            ));
                            this.note_status_is_error = true;
                        } else {
                            this.note_status_message = None;
                            this.note_status_is_error = false;
                        }
                        this.note_dirty = true;
                        this.note_last_edit = Some(std::time::Instant::now());
                        this.start_note_autosave_waker(cx);
                        cx.notify();
                    }
                    InputEvent::PressEnter => {
                        if let Some(username) = this.state.username.clone() {
                            this.note_dirty = false;
                            this.note_last_edit = None;
                            this.save_note_from_input(&username, cx);
                        }
                        if let Some(focus_handle) = this.note_dismiss_focus.clone() {
                            let entity_id = cx.entity_id();
                            let _ = cx.with_window(entity_id, |window, app| {
                                window.focus(&focus_handle, app);
                            });
                        }
                    }
                    InputEvent::Blur if this.note_dirty => {
                        this.note_dirty = false;
                        this.note_last_edit = None;
                        if let Some(username) = this.state.username.clone() {
                            this.save_note_from_input(&username, cx);
                        }
                    }
                    _ => {}
                });

            self.note_input = Some(input);
            self.subscriptions.push(subscription);
        }

        if self.note_input_bound_user.as_deref() != Some(username) {
            if let Some(input) = self.note_input.clone() {
                input.update(cx, |state, cx| {
                    state.set_value(current_note.to_string(), window, cx);
                });
            }
            self.note_input_bound_user = Some(username.to_string());
            self.note_status_message = None;
            self.note_status_is_error = false;
        }
    }

    /// Persist the note input. Auto-save runs constantly, so success is
    /// silent; only failures surface a status line.
    fn save_note_from_input(&mut self, username: &str, cx: &mut Context<Self>) {
        let Some(input) = self.note_input.clone() else {
            return;
        };
        let note = truncate_note_input(input.read(cx).value());
        let note = note.trim().to_string();

        let result = if note.is_empty() {
            backend::user_notes::delete_user_note(username).map(|_| ())
        } else {
            backend::user_notes::set_user_note(username, note).map(|_| ())
        };

        match result {
            Ok(()) => {
                self.note_status_message = None;
                self.note_status_is_error = false;
            }
            Err(error) => {
                self.note_status_message = Some(error);
                self.note_status_is_error = true;
            }
        }

        cx.notify();
    }

    /// Debounced auto-save loop for the persistent note box.
    fn start_note_autosave_waker(&mut self, cx: &mut Context<Self>) {
        if self.note_autosave_waker_running {
            return;
        }
        self.note_autosave_waker_running = true;
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(NOTE_AUTOSAVE_DEBOUNCE_MS / 2))
                    .await;

                let keep_running = this
                    .update(cx, |card, cx| {
                        if !card.note_dirty {
                            card.note_autosave_waker_running = false;
                            return false;
                        }
                        let idle_long_enough = card
                            .note_last_edit
                            .map(|at| at.elapsed().as_millis() as u64 >= NOTE_AUTOSAVE_DEBOUNCE_MS)
                            .unwrap_or(true);
                        if idle_long_enough {
                            card.note_dirty = false;
                            card.note_autosave_waker_running = false;
                            if let Some(username) = card.state.username.clone() {
                                card.save_note_from_input(&username, cx);
                            }
                            return false;
                        }
                        true
                    })
                    .unwrap_or(false);

                if !keep_running {
                    break;
                }
            }
        })
        .detach();
    }
}

impl Render for UserCard {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.state.is_open {
            self.ban_confirmation = None;
            self.metadata_refresh_task = None;
            return div().into_any_element();
        }

        self.start_metadata_refresh_waker(cx);

        let username = self.state.username.clone().unwrap_or_default();
        let hide_user_details = backend::streamer_mode::should_hide_user_card_details();
        let hide_mod_actions = backend::streamer_mode::should_hide_mod_actions();
        let twitch_profile = if hide_user_details {
            None
        } else {
            backend::twitch_api::get_cached_user_profile(&username)
        };
        let fallback_display_name = self
            .state
            .display_name
            .clone()
            .or_else(|| self.user_context.latest_display_name.clone())
            .or_else(|| {
                twitch_profile
                    .as_ref()
                    .map(|profile| profile.display_name.clone())
                    .filter(|name| !name.trim().is_empty())
            })
            .unwrap_or_else(|| username.clone());
        let target_user_id = self.resolved_user_id(twitch_profile.as_ref());
        let twitch_block_action = if hide_user_details || hide_mod_actions {
            None
        } else {
            target_user_id.as_deref().and_then(|user_id| {
                backend::blocked_users::block_action_state(user_id, &username).map(|blocked| {
                    TwitchBlockAction {
                        user: backend::blocked_users::BlockedUser {
                            user_id: user_id.to_string(),
                            user_login: username.clone(),
                            display_name: fallback_display_name.clone(),
                        },
                        blocked,
                    }
                })
            })
        };
        let seven_tv_cosmetics = target_user_id
            .as_deref()
            .and_then(backend::cosmetics::get_cached_user_cosmetics);
        let show_7tv_paints = backend::settings::get_show_seventv_name_paints();
        let show_7tv_badges = backend::settings::get_show_seventv_cosmetic_badges();
        let show_7tv_avatars = backend::settings::get_show_seventv_profile_avatars();
        let display_name = if show_7tv_paints {
            cosmetic_display_name(&fallback_display_name, seven_tv_cosmetics.as_ref())
        } else {
            fallback_display_name.clone()
        };
        let user_color = if show_7tv_paints {
            cosmetic_user_color(seven_tv_cosmetics.as_ref(), self.get_user_color())
        } else {
            self.get_user_color()
        };
        let seven_tv_avatar = show_7tv_avatars
            .then(|| {
                seven_tv_cosmetics.as_ref().and_then(|cosmetics| {
                    cosmetics
                        .avatar_url
                        .clone()
                        .map(|url| (url, cosmetics.avatar_is_animated))
                })
            })
            .flatten();
        let (avatar_url, avatar_is_animated) = match seven_tv_avatar {
            Some((url, is_animated)) => (Some(url), is_animated),
            None => (
                twitch_profile
                    .as_ref()
                    .and_then(|profile| profile.profile_image_url.clone()),
                false,
            ),
        };
        let channel = self.state.channel.clone().unwrap_or_default();
        let can_view_follow_metadata = Self::can_view_follow_metadata(&channel);
        let can_view_subscription_metadata =
            Self::can_view_subscription_metadata(&channel, &username);
        let follow_info = if hide_user_details
            || !can_view_follow_metadata
            || (!backend::settings::get_show_follow_date()
                && !backend::settings::get_show_follower_count())
        {
            None
        } else {
            backend::twitch_api::get_cached_user_follow(
                &channel,
                &username,
                target_user_id.as_deref(),
            )
        };
        let subscription_info = if hide_user_details || !can_view_subscription_metadata {
            None
        } else {
            backend::twitch_api::get_cached_user_subscription(
                &channel,
                &username,
                target_user_id.as_deref(),
            )
        };
        let profile_status = if hide_user_details {
            backend::twitch_api::UserCardFetchStatus {
                state: backend::twitch_api::UserCardFetchState::Idle,
                message: None,
                updated_at: std::time::Instant::now(),
                stale: false,
                auth_generation: backend::auth::credential_generation(),
            }
        } else {
            backend::twitch_api::get_user_profile_status(&username)
        };
        let follow_status = if hide_user_details
            || !can_view_follow_metadata
            || (!backend::settings::get_show_follow_date()
                && !backend::settings::get_show_follower_count())
        {
            backend::twitch_api::UserCardFetchStatus {
                state: backend::twitch_api::UserCardFetchState::Idle,
                message: None,
                updated_at: std::time::Instant::now(),
                stale: false,
                auth_generation: backend::auth::credential_generation(),
            }
        } else {
            backend::twitch_api::get_user_follow_status(
                &channel,
                &username,
                target_user_id.as_deref(),
            )
        };
        let subscription_status = if hide_user_details || !can_view_subscription_metadata {
            backend::twitch_api::UserCardFetchStatus {
                state: backend::twitch_api::UserCardFetchState::Idle,
                message: None,
                updated_at: std::time::Instant::now(),
                stale: false,
                auth_generation: backend::auth::credential_generation(),
            }
        } else {
            backend::twitch_api::get_user_subscription_status(
                &channel,
                &username,
                target_user_id.as_deref(),
            )
        };
        let note_entry = if hide_user_details {
            None
        } else {
            backend::user_notes::get_user_note_entry(&username)
        };
        let current_note = note_entry
            .as_ref()
            .map(|note| note.note.as_str())
            .unwrap_or_default();
        if !hide_user_details {
            self.ensure_note_input(window, cx, &username, current_note);
        }
        backend::queue_bttv_pro_badge_lookup(&channel, &username, target_user_id.as_deref());
        let cosmetic_badge = show_7tv_badges
            .then(|| {
                seven_tv_cosmetics
                    .as_ref()
                    .and_then(|cosmetics| cosmetics.badge.clone())
            })
            .flatten();
        let mut badges = if self.state.badges.is_empty() {
            self.user_context.latest_badges.clone()
        } else {
            self.state.badges.clone()
        };
        for badge_key in
            backend::get_external_badge_keys(&channel, &username, target_user_id.as_deref())
        {
            if badges
                .iter()
                .any(|existing| backend::badges::badge_key_matches(existing, &badge_key))
            {
                continue;
            }
            badges.push(badge_key);
        }
        let header_display_name = if hide_user_details {
            "User hidden".to_string()
        } else {
            display_name.clone()
        };
        let header_username = if hide_user_details {
            "hidden".to_string()
        } else {
            username.clone()
        };
        let header_user_color = if hide_user_details {
            colors::text_secondary()
        } else {
            user_color
        };
        let header_badges = if hide_user_details {
            Vec::new()
        } else {
            badges.clone()
        };
        let header_avatar_url = if hide_user_details { None } else { avatar_url };
        let header_avatar_is_animated = !hide_user_details && avatar_is_animated;
        let header_cosmetic_badge = if hide_user_details {
            None
        } else {
            cosmetic_badge
        };
        let header_is_live = !hide_user_details
            && backend::twitch_api::get_cached_user_live_status(&username) == Some(true);
        self.ensure_recent_rows(&channel, hide_user_details, window.scale_factor());

        let animation_urls_key = AnimationUrlsKey {
            rows_generation: self.recent_rows_generation,
            avatar_url: if header_avatar_is_animated {
                header_avatar_url.clone()
            } else {
                None
            },
            badge_url: header_cosmetic_badge
                .as_ref()
                .filter(|badge| badge.is_animated)
                .map(|badge| badge.url.clone()),
        };
        if self.animation_urls_key.as_ref() != Some(&animation_urls_key)
            || !self.cosmetic_animation_waker_running
        {
            let mut urls = Vec::new();
            if let Some(url) = animation_urls_key.avatar_url.as_ref() {
                urls.push(url.clone());
            }
            if let Some(url) = animation_urls_key.badge_url.as_ref() {
                urls.push(url.clone());
            }
            for url in &self.recent_row_animation_urls {
                if urls.len() >= 64 {
                    break;
                }
                if !urls.contains(url) {
                    urls.push(url.clone());
                }
            }
            self.animation_urls_key = Some(animation_urls_key);
            self.update_cosmetic_animation_waker(urls, cx);
        }

        let header_user_id = if hide_user_details {
            None
        } else {
            self.resolved_user_id(twitch_profile.as_ref())
        };
        let can_view_follow_date = backend::settings::get_show_follow_date()
            && backend::is_authenticated()
            && !backend::irc::is_anonymous_mode()
            && backend::irc::can_moderate_channel(&channel)
            && !(follow_status.auth_generation == backend::auth::credential_generation()
                && matches!(
                    follow_status.state,
                    backend::twitch_api::UserCardFetchState::Forbidden
                        | backend::twitch_api::UserCardFetchState::AuthRequired
                ));
        let mut header_meta_lines: Vec<String> = Vec::new();
        if !hide_user_details {
            let mut first_line: Vec<String> = Vec::new();
            if backend::settings::get_show_follower_count()
                && let Some(count) = follow_info.as_ref().and_then(|info| info.follower_count)
            {
                first_line.push(format!("Followers: {}", format_count_compact(count)));
            }
            if let Some(created) = twitch_profile
                .as_ref()
                .and_then(|profile| profile.created_at.as_deref())
                .and_then(format_twitch_date)
            {
                first_line.push(format!("Created: {}", created));
            }
            header_meta_lines.push(first_line.join("   "));
            if can_view_follow_date {
                let followed = follow_info
                    .as_ref()
                    .and_then(|info| info.followed_at.as_deref())
                    .and_then(format_twitch_date);
                header_meta_lines.push(
                    followed
                        .map(|date| format!("\u{2665} Following since {date}"))
                        .unwrap_or_default(),
                );
            }
            let subscription =
                subscription_summary_from_live_and_badges(subscription_info.as_ref(), &badges);
            if can_view_subscription_metadata || subscription.is_some() {
                header_meta_lines.push(
                    subscription
                        .map(|summary| format!("\u{2605} {summary}"))
                        .unwrap_or_default(),
                );
            }
            if backend::settings::get_show_pronouns() {
                header_meta_lines.push(
                    backend::pronouns::get_pronouns(&username)
                        .map(|pronouns| format!("Pronouns: {pronouns}"))
                        .unwrap_or_default(),
                );
            }
        }
        // Late empty/forbidden results must not shrink the card after its first frame.
        let header_meta_reserved_lines = if hide_user_details {
            0
        } else {
            *self
                .header_meta_reserved_lines
                .get_or_insert(header_meta_lines.len())
        };
        let header_bio = if hide_user_details {
            None
        } else {
            twitch_profile
                .as_ref()
                .and_then(|profile| profile.description.as_deref())
                .map(str::trim)
                .filter(|description| !description.is_empty())
                .map(|description| truncate_text(description, 140))
        };
        let viewport = window.viewport_size();
        let card_fit =
            crate::utils::fit_overlay_to_viewport(viewport, Sizes::USER_CARD_WIDTH, 450.0, 8.0);
        if !self.hosted_in_window {
            if !self.state.x.is_finite() || !self.state.y.is_finite() {
                self.state.x = (f32::from(viewport.width) - card_fit.width) / 2.0;
                self.state.y = (f32::from(viewport.height) - card_fit.height) / 2.0;
            }
            self.state.x = crate::utils::clamp_overlay_axis(
                self.state.x,
                card_fit.width,
                f32::from(viewport.width),
                card_fit.margin_x,
            );
            self.state.y = crate::utils::clamp_overlay_axis(
                self.state.y,
                card_fit.height,
                f32::from(viewport.height),
                card_fit.margin_y,
            );
        }

        let card = div()
            .id("user-card")
            .bg(colors::bg_elevated())
            .overflow_hidden()
            .flex()
            .flex_col();
        let card = if self.hosted_in_window {
            card.size_full()
                .border_1()
                .border_color(colors::border())
                .on_mouse_move(cx.listener(|this, _event: &MouseMoveEvent, _window, cx| {
                    if crate::utils::should_start_drag() {
                        #[cfg(target_os = "windows")]
                        crate::utils::start_window_drag();
                        #[cfg(not(target_os = "windows"))]
                        _window.start_window_move();
                    }
                    this.is_dragging = false;
                    cx.stop_propagation();
                }))
                .on_mouse_up(
                    MouseButton::Left,
                    cx.listener(|this, _, _window, cx| {
                        crate::utils::cancel_pending_drag();
                        this.is_dragging = false;
                        cx.stop_propagation();
                    }),
                )
        } else {
            card.absolute()
                .left(px(self.state.x))
                .top(px(self.state.y))
                .w(px(card_fit.width))
                .max_h(px(card_fit.height))
                .occlude()
                .rounded(Radii::lg())
                .border_1()
                .border_color(colors::accent().opacity(0.2))
                .shadow_md()
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_mouse_down(MouseButton::Right, |_, _, cx| cx.stop_propagation())
                .on_mouse_down(MouseButton::Middle, |_, _, cx| cx.stop_propagation())
                .on_mouse_up(
                    MouseButton::Left,
                    cx.listener(|this, _, _window, cx| {
                        this.is_dragging = false;
                        cx.stop_propagation();
                    }),
                )
                .on_mouse_up(MouseButton::Right, |_, _, cx| cx.stop_propagation())
                .on_mouse_up(MouseButton::Middle, |_, _, cx| cx.stop_propagation())
                .on_scroll(|_, _, cx| cx.stop_propagation())
                .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _window, cx| {
                    cx.stop_propagation();
                    if this.is_dragging {
                        if event.pressed_button != Some(MouseButton::Left) {
                            this.is_dragging = false;
                            cx.notify();
                            return;
                        }
                        let new_x: f32 = event.position.x.into();
                        let new_y: f32 = event.position.y.into();
                        let offset_x: f32 = this.drag_offset.x.into();
                        let offset_y: f32 = this.drag_offset.y.into();
                        this.state.x = new_x - offset_x;
                        this.state.y = new_y - offset_y;
                        cx.notify();
                    }
                }))
        };
        let card = card
            .child(self.render_header(
                &header_display_name,
                &header_username,
                header_user_color,
                &header_badges,
                header_avatar_url,
                header_avatar_is_animated,
                header_cosmetic_badge,
                header_is_live,
                &channel,
                header_user_id,
                &header_meta_lines,
                header_meta_reserved_lines,
                header_bio,
                (!hide_user_details).then_some(matches!(
                    profile_status.state,
                    backend::twitch_api::UserCardFetchState::Idle
                        | backend::twitch_api::UserCardFetchState::Loading
                )),
                cx,
            ))
            .child(self.render_user_info(
                window,
                &username,
                seven_tv_profile_url(seven_tv_cosmetics.as_ref()),
                twitch_profile.is_some(),
                follow_info.is_some(),
                subscription_info.is_some(),
                &profile_status,
                &follow_status,
                &subscription_status,
                hide_user_details,
                twitch_block_action,
                cx,
            ))
            .child(self.render_recent_messages(hide_user_details, cx))
            .when(
                !hide_mod_actions && self.can_moderate_current_target(),
                |el| el.child(self.render_mod_actions(&channel, &username, cx)),
            );

        let card = if self.is_dragging && !self.hosted_in_window {
            div()
                .absolute()
                .inset_0()
                .occlude()
                .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _window, cx| {
                    cx.stop_propagation();
                    if event.pressed_button != Some(MouseButton::Left) {
                        this.is_dragging = false;
                        cx.notify();
                        return;
                    }
                    let new_x: f32 = event.position.x.into();
                    let new_y: f32 = event.position.y.into();
                    let offset_x: f32 = this.drag_offset.x.into();
                    let offset_y: f32 = this.drag_offset.y.into();
                    this.state.x = new_x - offset_x;
                    this.state.y = new_y - offset_y;
                    cx.notify();
                }))
                .on_mouse_up(
                    MouseButton::Left,
                    cx.listener(|this, _, _window, cx| {
                        this.is_dragging = false;
                        cx.notify();
                        cx.stop_propagation();
                    }),
                )
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_mouse_down(MouseButton::Right, |_, _, cx| cx.stop_propagation())
                .on_mouse_down(MouseButton::Middle, |_, _, cx| cx.stop_propagation())
                .on_mouse_up(MouseButton::Right, |_, _, cx| cx.stop_propagation())
                .on_mouse_up(MouseButton::Middle, |_, _, cx| cx.stop_propagation())
                .on_scroll(|_, _, cx| cx.stop_propagation())
                .child(card)
                .into_any_element()
        } else {
            card.into_any_element()
        };
        if let Some(confirmation) = self.ban_confirmation.as_ref()
            && confirmation.modal.read(cx).state.is_open
        {
            div()
                .size_full()
                .relative()
                .when(!self.hosted_in_window, |el| el.absolute().inset_0())
                .on_key_down(|_, _, cx| cx.stop_propagation())
                .child(card)
                .child(confirmation.modal.clone())
                .into_any_element()
        } else {
            card
        }
    }
}

impl UserCard {
    #[allow(clippy::too_many_arguments)]
    fn render_header(
        &self,
        display_name: &str,
        username: &str,
        user_color: Hsla,
        badges: &[String],
        avatar_url: Option<String>,
        avatar_is_animated: bool,
        cosmetic_badge: Option<SevenTvBadge>,
        is_live: bool,
        channel: &str,
        user_id: Option<String>,
        meta_lines: &[String],
        reserved_meta_lines: usize,
        bio: Option<String>,
        profile_pending: Option<bool>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let display_name = display_name.to_string();
        let username = username.to_string();
        let login_for_copy = username.clone();
        let id_for_copy = user_id.clone();
        let bio_loading = bio.is_none() && profile_pending == Some(true);

        let card_x = self.state.x;
        let card_y = self.state.y;

        div()
            .px_3()
            .py_2()
            .flex_none()
            .bg(colors::bg_plate())
            .border_b_1()
            .border_color(colors::border())
            .flex()
            .items_start()
            .gap_2p5()
            .cursor(if self.hosted_in_window {
                CursorStyle::OpenHand
            } else {
                CursorStyle::Arrow
            })
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event: &MouseDownEvent, _window, cx| {
                    if this.hosted_in_window {
                        crate::utils::arm_window_drag();
                    } else {
                        this.is_dragging = true;
                        let mouse_x: f32 = event.position.x.into();
                        let mouse_y: f32 = event.position.y.into();
                        this.drag_offset = point(px(mouse_x - card_x), px(mouse_y - card_y));
                    }
                    cx.stop_propagation();
                }),
            )
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _window, cx| {
                    if this.hosted_in_window {
                        crate::utils::cancel_pending_drag();
                    }
                    this.is_dragging = false;
                    cx.stop_propagation();
                }),
            )
            .child(render_user_card_avatar(
                avatar_url.as_deref(),
                avatar_is_animated,
                &display_name,
                user_color,
            ))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap_0p5()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .flex_wrap()
                            .gap_1()
                            .child(
                                div()
                                    .id("user-card-copy-name")
                                    .text_sm()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(user_color)
                                    .cursor_pointer()
                                    .hover(|s| s.opacity(0.8))
                                    .on_activate({
                                        let login = login_for_copy.clone();
                                        move |_, _, _| copy_text(&login)
                                    })
                                    .child(display_name.clone()),
                            )
                            .when(is_live, |row| {
                                row.child(
                                    div()
                                        .px_1p5()
                                        .py_0p5()
                                        .rounded(Radii::sm())
                                        .border_1()
                                        .border_color(colors::error().opacity(0.45))
                                        .bg(colors::error().opacity(0.14))
                                        .text_xs()
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .text_color(colors::error())
                                        .child("LIVE"),
                                )
                            })
                            .children(
                                badges
                                    .iter()
                                    .filter(|badge| {
                                        backend::settings::should_render_badge_key(badge)
                                    })
                                    .take(4)
                                    .map(|badge| render_badge(badge, channel)),
                            )
                            .when_some(cosmetic_badge, |el, badge| {
                                el.child(render_7tv_badge(&badge))
                            }),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(
                                div()
                                    .id("user-card-copy-login")
                                    .text_xs()
                                    .text_color(colors::text_muted())
                                    .cursor_pointer()
                                    .hover(|s| s.text_color(colors::text_secondary()))
                                    .on_activate({
                                        let login = login_for_copy.clone();
                                        move |_, _, _| copy_text(&login)
                                    })
                                    .child(format!("@{}", username)),
                            )
                            .when_some(id_for_copy, |el, id| {
                                el.child(
                                    div()
                                        .id("user-card-copy-id")
                                        .text_xs()
                                        .text_color(colors::text_muted())
                                        .cursor_pointer()
                                        .hover(|s| s.text_color(colors::text_secondary()))
                                        .on_activate({
                                            let id_text = id.clone();
                                            move |_, _, _| copy_text(&id_text)
                                        })
                                        .child(format!("ID: {} \u{29C9}", id)),
                                )
                            }),
                    )
                    .when(profile_pending.is_some(), |el| {
                        let bio_tooltip = bio.clone();
                        el.child(
                            div()
                                .min_h(px(reserved_meta_lines as f32 * 18.0 + 32.0))
                                .flex_none()
                                .flex()
                                .flex_col()
                                .gap(px(2.0))
                                .children(
                                    meta_lines
                                        .iter()
                                        .enumerate()
                                        .filter(|(_, line)| !line.is_empty())
                                        .map(|(index, line)| {
                                            div()
                                                .id(SharedString::from(format!(
                                                    "user-card-meta-{index}"
                                                )))
                                                .h(px(16.0))
                                                .flex_none()
                                                .line_height(px(16.0))
                                                .text_ellipsis()
                                                .text_xs()
                                                .text_color(colors::text_secondary())
                                                .child(line.clone())
                                                .on_hover(text_tooltip_on_hover(
                                                    format!("user-card-meta-{index}"),
                                                    line.clone(),
                                                ))
                                        }),
                                )
                                .child(
                                    div()
                                        .id("user-card-bio")
                                        .h(px(32.0))
                                        .flex_none()
                                        .line_height(px(16.0))
                                        .line_clamp(2)
                                        .text_xs()
                                        .text_color(colors::text_muted())
                                        .when_some(bio, |el, bio| el.child(bio))
                                        .when_some(bio_tooltip, |el, bio| {
                                            el.on_hover(text_tooltip_on_hover(
                                                "user-card-bio".to_string(),
                                                bio,
                                            ))
                                        })
                                        .when(bio_loading, |el| {
                                            el.child(
                                                div()
                                                    .mt(px(6.0))
                                                    .h(px(4.0))
                                                    .w_full()
                                                    .rounded(px(2.0))
                                                    .bg(colors::bg_hover()),
                                            )
                                        }),
                                ),
                        )
                    }),
            )
            .child(
                div()
                    .id("user-card-close")
                    .role(Role::Button)
                    .aria_label("Close user card")
                    .on_hover(crate::components::overlays::text_tooltip_on_hover(
                        "user-card-close".to_string(),
                        "Close".to_string(),
                    ))
                    .size(px(Sizes::BUTTON_SM))
                    .rounded(Radii::sm())
                    .cursor_pointer()
                    .flex()
                    .items_center()
                    .justify_center()
                    .hover(|s| s.bg(colors::bg_hover()))
                    .on_activate(cx.listener(|this, _, _window, cx| {
                        this.state.close();
                        cx.notify();
                    }))
                    .child(
                        div()
                            .text_sm()
                            .text_color(colors::text_secondary())
                            .child(crate::icons::CLOSE_GLYPH),
                    ),
            )
    }

    #[allow(clippy::too_many_arguments)]
    fn render_user_info(
        &self,
        window: &mut Window,
        username: &str,
        seven_tv_profile_url: Option<String>,
        has_profile: bool,
        has_follow: bool,
        has_subscription: bool,
        profile_status: &backend::twitch_api::UserCardFetchStatus,
        follow_status: &backend::twitch_api::UserCardFetchStatus,
        subscription_status: &backend::twitch_api::UserCardFetchStatus,
        hide_user_details: bool,
        twitch_block_action: Option<TwitchBlockAction>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let username_clone = username.to_string();
        let metadata_status_rows = if hide_user_details {
            Vec::new()
        } else {
            vec![
                build_metadata_status_row("Profile", profile_status, has_profile)
                    .filter(|row| matches!(row.tone, InlineStatusTone::Warning)),
                build_metadata_status_row("Follow", follow_status, has_follow),
                build_metadata_status_row("Subscription", subscription_status, has_subscription),
            ]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
        };
        div()
            .px_3()
            .py_1p5()
            .border_b_1()
            .border_color(colors::border())
            .flex()
            .flex_col()
            .gap_1()
            .when(hide_user_details, |el| {
                el.child(render_streamer_mode_placeholder(
                    "User details hidden by streamer mode",
                ))
            })
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap_1()
                    .child(self.render_mention_button(&username_clone, cx))
                    .child(self.render_whisper_button(&username_clone, cx))
                    .when_some(twitch_block_action, |el, action| {
                        el.child(self.render_twitch_block_button(action, cx))
                    })
                    .when(!hide_user_details, |el| {
                        el.child(self.render_profile_button(&username_clone, cx))
                            .child(self.render_open_chat_button(&username_clone, cx))
                            .when_some(seven_tv_profile_url, |el, url| {
                                el.child(self.render_seven_tv_button(url, cx))
                            })
                    }),
            )
            .when(!hide_user_details, |el| {
                el.child(self.render_user_note_section(window, cx))
            })
            .when(!metadata_status_rows.is_empty(), |el| {
                el.child(
                    div().flex().flex_col().gap_1().children(
                        metadata_status_rows
                            .iter()
                            .cloned()
                            .map(render_metadata_status_row),
                    ),
                )
            })
            .when_some(self.note_status_message.clone(), |el, status| {
                el.child(render_inline_status(
                    &status,
                    if self.note_status_is_error {
                        InlineStatusTone::Warning
                    } else {
                        InlineStatusTone::Success
                    },
                ))
            })
    }

    fn render_mention_button(&self, username: &str, cx: &mut Context<Self>) -> AnyElement {
        let username = username.to_string();
        let channel = self.state.channel.clone().unwrap_or_default();

        self.render_secondary_action_button(
            "Mention",
            colors::text_primary(),
            cx.listener(move |this, _, _window, cx| {
                backend::push_ui_action(backend::UiAction::InsertMention {
                    username: username.clone(),
                    channel: channel.clone(),
                    source_split_id: None,
                });
                this.update_state(UserCardState::default());
                cx.notify();
            }),
        )
    }

    fn render_whisper_button(
        &self,
        username: &str,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let username = username.to_string();
        let channel = self.state.channel.clone();
        let color = colors::accent();

        div()
            .id("user-card-whisper")
            .px_2()
            .py_1()
            .rounded(Radii::sm())
            .bg(color.opacity(0.1))
            .cursor_pointer()
            .hover(|s| s.bg(color.opacity(0.2)))
            .on_activate(cx.listener({
                let username = username.clone();
                let channel = channel.clone();
                move |this, _, _window, cx| {
                    backend::push_ui_action(backend::UiAction::ComposeWhisper {
                        username: username.clone(),
                        channel: channel.clone(),
                    });
                    this.update_state(UserCardState::default());
                    cx.notify();
                }
            }))
            .flex()
            .items_center()
            .justify_center()
            .gap_1p5()
            .child(
                div()
                    .text_xs()
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(color)
                    .child("Whisper"),
            )
    }

    fn render_twitch_block_button(
        &self,
        action: TwitchBlockAction,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let label = if action.blocked { "Unblock" } else { "Block" };
        let color = if action.blocked {
            colors::success()
        } else {
            colors::error()
        };

        self.render_secondary_action_button(
            label,
            color,
            cx.listener(move |this, _, window, cx| {
                let session = backend::blocked_users::capture_active_session();
                let hosted_in_window = this.hosted_in_window;
                this.update_state(UserCardState::default());
                if hosted_in_window {
                    window.remove_window();
                }

                let session = match session {
                    Ok(session) if action.user.user_id != session.owner_user_id() => session,
                    Ok(_) => {
                        backend::push_ui_action(backend::UiAction::ShowWarningToast {
                            message: "Action unavailable".to_string(),
                            detail: Some(
                                "The active Twitch account cannot target itself.".to_string(),
                            ),
                        });
                        cx.notify();
                        return;
                    }
                    Err(error) => {
                        backend::push_ui_action(backend::UiAction::ShowWarningToast {
                            message: "Action unavailable".to_string(),
                            detail: Some(error),
                        });
                        cx.notify();
                        return;
                    }
                };

                if action.blocked {
                    backend::blocked_users::set_block_state_async_for_session(
                        session,
                        action.user.clone(),
                        false,
                    );
                } else {
                    backend::push_ui_action(backend::UiAction::ConfirmBlockUser {
                        user_id: action.user.user_id.clone(),
                        username: action.user.user_login.clone(),
                        display_name: action.user.display_name.clone(),
                        expected_owner_user_id: session.owner_user_id().to_string(),
                    });
                }
                cx.notify();
            }),
        )
    }

    fn render_profile_button(&self, username: &str, cx: &mut Context<Self>) -> AnyElement {
        let url = format!("https://twitch.tv/{}", username);

        self.render_secondary_action_button(
            "Profile",
            colors::text_primary(),
            cx.listener({
                let url = url.clone();
                move |this, _, _window, cx| {
                    queue_confirm_open_link(url.clone());
                    this.update_state(UserCardState::default());
                    cx.notify();
                }
            }),
        )
    }

    fn render_open_chat_button(&self, username: &str, cx: &mut Context<Self>) -> AnyElement {
        let url = format!("https://www.twitch.tv/popout/{username}/chat");

        self.render_secondary_action_button(
            "Chat",
            colors::text_primary(),
            cx.listener({
                let url = url.clone();
                move |this, _, _window, cx| {
                    queue_confirm_open_link(url.clone());
                    this.update_state(UserCardState::default());
                    cx.notify();
                }
            }),
        )
    }

    fn render_seven_tv_button(&self, url: String, cx: &mut Context<Self>) -> AnyElement {
        self.render_secondary_action_button(
            "7TV",
            colors::text_primary(),
            cx.listener(move |this, _, _window, cx| {
                queue_confirm_open_link(url.clone());
                this.update_state(UserCardState::default());
                cx.notify();
            }),
        )
    }

    fn render_secondary_action_button<F>(
        &self,
        label: &'static str,
        text_color: Hsla,
        on_click: F,
    ) -> AnyElement
    where
        F: Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    {
        div()
            .id(label)
            .px_2()
            .py_0p5()
            .rounded(Radii::sm())
            .border_1()
            .border_color(colors::border())
            .bg(colors::bg_surface())
            .cursor_pointer()
            .hover(|s| s.bg(colors::bg_hover()))
            .on_activate(on_click)
            .flex()
            .items_center()
            .justify_center()
            .gap_1p5()
            .child(
                div()
                    .text_xs()
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(text_color)
                    .child(label),
            )
            .into_any_element()
    }

    fn render_user_note_section(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let is_focused = self
            .note_input
            .as_ref()
            .is_some_and(|input| input.read(cx).focus_handle().is_focused(window));
        let editor = if let Some(input) = self.note_input.as_ref() {
            let focus_handle = input.read(cx).focus_handle().clone();

            apply_standard_input_chrome(
                div()
                    .id("user-card-note-shell")
                    .flex_1()
                    .min_w_0()
                    .min_h(px(26.0))
                    .px_2()
                    .cursor_text()
                    .flex()
                    .items_center()
                    .on_mouse_down(MouseButton::Left, move |_event, window, cx| {
                        window.focus(&focus_handle, cx);
                        window.prevent_default();
                    }),
                is_focused,
            )
            .child(
                div()
                    .flex_1()
                    .text_xs()
                    .text_color(colors::text_primary())
                    .child(
                        Input::new(input)
                            .appearance(false)
                            .bordered(false)
                            .focus_bordered(false),
                    ),
            )
            .into_any_element()
        } else {
            div()
                .flex_1()
                .text_xs()
                .text_color(colors::text_muted())
                .child("Loading note editor…")
                .into_any_element()
        };

        let dismiss_focus = self.note_dismiss_focus.clone();

        div()
            .flex()
            .items_center()
            .gap_1()
            .when_some(dismiss_focus.as_ref(), |row, focus_handle| {
                row.track_focus(focus_handle)
            })
            .child(editor)
            .when(is_focused, |row| {
                row.child(
                    div()
                        .id("user-card-note-done")
                        .px_2()
                        .min_h(px(26.0))
                        .rounded(Radii::sm())
                        .cursor_pointer()
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_xs()
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(colors::accent_text())
                        .hover(|style| style.bg(colors::accent_transparent(0.12)))
                        .on_activate(cx.listener(move |this, _event, window, cx| {
                            this.note_dirty = false;
                            this.note_last_edit = None;
                            if let Some(username) = this.state.username.clone() {
                                this.save_note_from_input(&username, cx);
                            }
                            if let Some(focus_handle) = this.note_dismiss_focus.as_ref() {
                                window.focus(focus_handle, cx);
                            } else {
                                window.blur(cx);
                            }
                            cx.notify();
                        }))
                        .child("Done"),
                )
            })
    }

    fn render_recent_messages(
        &self,
        hide_user_details: bool,
        _cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let messages_count = self.user_context.recent_messages.len();

        div()
            .flex_1()
            .min_h_0()
            .when(!self.hosted_in_window, |el| el.max_h(px(200.0)))
            .overflow_hidden()
            .flex()
            .flex_col()
            .child(
                div()
                    .px_3()
                    .py_1p5()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(
                        div().flex().items_center().gap_1p5().child(
                            div()
                                .text_xs()
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(colors::text_primary())
                                .child("Recent Messages"),
                        ),
                    )
                    .when(!hide_user_details, |el| {
                        el.child(
                            div()
                                .px_1p5()
                                .py_0p5()
                                .rounded(Radii::xl())
                                .bg(colors::bg_hover())
                                .text_xs()
                                .text_color(colors::text_secondary())
                                .child(messages_count.to_string()),
                        )
                    }),
            )
            .child(
                div()
                    .id("user-card-messages")
                    .mx_3()
                    .mb_2()
                    .rounded(Radii::sm())
                    .bg(colors::bg_surface())
                    .overflow_y_scroll()
                    .when(self.hosted_in_window, |el| el.flex_1().min_h_0())
                    .when(!self.hosted_in_window, |el| el.max_h(px(150.0)))
                    .when(hide_user_details, |el| {
                        el.child(render_streamer_mode_placeholder(
                            "Recent messages hidden by streamer mode",
                        ))
                    })
                    .when(
                        !hide_user_details && self.user_context.recent_messages.is_empty(),
                        |el| {
                            el.child(
                                div()
                                    .p_3()
                                    .text_xs()
                                    .text_color(colors::text_secondary())
                                    .text_center()
                                    .child("No messages found"),
                            )
                        },
                    )
                    .when(!hide_user_details && !self.recent_rows.is_empty(), |el| {
                        el.child(
                            div().children(self.recent_rows.iter().map(render_rich_message_row)),
                        )
                    }),
            )
    }

    fn render_mod_actions(
        &self,
        channel: &str,
        username: &str,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let channel = channel.to_string();
        let username = username.to_string();
        let is_banned =
            crate::backend::components::mod_log::is_user_likely_banned(&channel, &username);

        const TIMEOUTS: [(&str, u64); 4] =
            [("Purge", 1), ("10m", 600), ("1h", 3600), ("1d", 86400)];

        let buttons = div()
            .px_2()
            .py_1p5()
            .border_t_1()
            .border_color(colors::border())
            .flex()
            .items_center()
            .gap_1()
            .child(if is_banned {
                render_compact_mod_button(
                    "Unban",
                    colors::success(),
                    channel.clone(),
                    username.clone(),
                    format!("/unban {}", username),
                    cx,
                )
            } else {
                render_compact_mod_button(
                    "Ban",
                    colors::error(),
                    channel.clone(),
                    username.clone(),
                    format!("/ban {}", username),
                    cx,
                )
            })
            .child(
                div()
                    .flex_1()
                    .flex()
                    .items_center()
                    .justify_center()
                    .gap_0p5()
                    .children(TIMEOUTS.into_iter().map(|(label, seconds)| {
                        render_compact_mod_button(
                            label,
                            colors::text_secondary(),
                            channel.clone(),
                            username.clone(),
                            format!("/timeout {} {}", username, seconds),
                            cx,
                        )
                    })),
            )
            .child(render_warn_button(&username, self.warn_prompt_open, cx));

        div().flex().flex_col().child(buttons).when_some(
            self.warn_input.clone().filter(|_| self.warn_prompt_open),
            |el, input| el.child(render_warn_prompt(input, cx)),
        )
    }
}

fn render_warn_button(target: &str, prompt_open: bool, cx: &mut Context<UserCard>) -> AnyElement {
    let color = colors::warning();
    div()
        .id(SharedString::from(format!("user-card-mod-Warn-{target}")))
        .px_1p5()
        .py_0p5()
        .min_w(px(26.0))
        .rounded(Radii::sm())
        .border_1()
        .border_color(color.opacity(0.4))
        .when(prompt_open, |el| el.bg(color.opacity(0.12)))
        .cursor_pointer()
        .flex()
        .items_center()
        .justify_center()
        .hover(move |s| s.bg(color.opacity(0.12)))
        .on_activate(cx.listener(move |this, _, window, cx| {
            if this.warn_prompt_open {
                this.warn_prompt_open = false;
                cx.notify();
            } else {
                this.open_warn_prompt(window, cx);
            }
        }))
        .child(div().text_xs().text_color(color).child("Warn"))
        .into_any_element()
}

fn render_warn_prompt(input: Entity<InputState>, cx: &mut Context<UserCard>) -> AnyElement {
    let has_reason = !input.read(cx).value().trim().is_empty();
    let focus_handle = input.read(cx).focus_handle().clone();
    let color = colors::warning();
    div()
        .px_2()
        .pb_1p5()
        .flex()
        .items_center()
        .gap_1()
        .capture_action(cx.listener(|this, _: &Escape, window, cx| {
            this.warn_prompt_open = false;
            if let Some(focus_handle) = this.host_focus.clone() {
                window.focus(&focus_handle, cx);
            }
            cx.notify();
            cx.stop_propagation();
        }))
        .child(
            apply_standard_input_chrome(
                div()
                    .id("user-card-warn-shell")
                    .flex_1()
                    .min_w_0()
                    .min_h(px(24.0))
                    .px_2()
                    .cursor_text()
                    .flex()
                    .items_center()
                    .on_mouse_down(MouseButton::Left, move |_event, window, cx| {
                        window.focus(&focus_handle, cx);
                        window.prevent_default();
                    }),
                true,
            )
            .child(
                div()
                    .flex_1()
                    .text_xs()
                    .text_color(colors::text_primary())
                    .child(
                        Input::new(&input)
                            .appearance(false)
                            .bordered(false)
                            .focus_bordered(false),
                    ),
            ),
        )
        .child(
            div()
                .id("user-card-warn-send")
                .px_2()
                .py_0p5()
                .rounded(Radii::sm())
                .border_1()
                .border_color(color.opacity(if has_reason { 0.6 } else { 0.2 }))
                .text_xs()
                .text_color(if has_reason {
                    color
                } else {
                    colors::text_muted()
                })
                .when(has_reason, |el| {
                    el.cursor_pointer()
                        .hover(move |s| s.bg(color.opacity(0.12)))
                })
                .on_activate(cx.listener(|this, _, _window, cx| this.submit_warn(cx)))
                .child("Send warning"),
        )
        .into_any_element()
}

/// Compact bordered mod button. Sends the command and keeps
/// the card open so follow-up actions (escalating timeouts etc.) are easy.
fn render_compact_mod_button(
    label: &'static str,
    color: Hsla,
    channel: String,
    target: String,
    command: String,
    cx: &mut Context<UserCard>,
) -> AnyElement {
    div()
        .id(SharedString::from(format!(
            "user-card-mod-{label}-{target}"
        )))
        .px_1p5()
        .py_0p5()
        .min_w(px(26.0))
        .rounded(Radii::sm())
        .border_1()
        .border_color(color.opacity(0.4))
        .cursor_pointer()
        .flex()
        .items_center()
        .justify_center()
        .hover(move |s| s.bg(color.opacity(0.12)))
        .on_activate(cx.listener(move |this, _, window, cx| {
            if label == "Ban" {
                this.open_ban_confirmation(&channel, &target, window, cx);
                return;
            }
            if crate::components::common::moderation_policy::recheck_moderation_target(
                &channel, &target,
            ) {
                backend::send_message_or_command_async(channel.clone(), command.clone());
            }
            cx.notify();
        }))
        .child(div().text_xs().text_color(color).child(label))
        .into_any_element()
}

fn format_twitch_date(timestamp: &str) -> Option<String> {
    let timestamp = timestamp.trim();
    if timestamp.is_empty() {
        return None;
    }

    timestamp
        .split_once('T')
        .map(|(date, _)| date.to_string())
        .or_else(|| Some(timestamp.to_string()))
}

fn format_count_compact(count: u64) -> String {
    if count >= 1_000_000 {
        format!("{:.1}M", count as f64 / 1_000_000.0)
    } else if count >= 1_000 {
        format!("{:.1}K", count as f64 / 1_000.0)
    } else {
        count.to_string()
    }
}

#[derive(Clone, Copy)]
enum InlineStatusTone {
    Neutral,
    Success,
    Warning,
}

#[derive(Clone)]
struct MetadataStatusRow {
    label: &'static str,
    message: String,
    tone: InlineStatusTone,
}

fn build_metadata_status_row(
    label: &'static str,
    status: &backend::twitch_api::UserCardFetchStatus,
    has_cached_value: bool,
) -> Option<MetadataStatusRow> {
    use backend::twitch_api::UserCardFetchState;

    let (message, tone) = match status.state {
        UserCardFetchState::Idle => return None,
        UserCardFetchState::Ready if !status.stale => return None,
        UserCardFetchState::Loading if has_cached_value => (
            "Refreshing cached data…".to_string(),
            InlineStatusTone::Neutral,
        ),
        UserCardFetchState::Loading => ("Loading…".to_string(), InlineStatusTone::Neutral),
        UserCardFetchState::Ready if status.stale => {
            ("Showing cached data".to_string(), InlineStatusTone::Neutral)
        }
        UserCardFetchState::Unavailable => (
            status
                .message
                .clone()
                .unwrap_or_else(|| "Unavailable".to_string()),
            InlineStatusTone::Warning,
        ),
        UserCardFetchState::Forbidden => (
            status
                .message
                .clone()
                .unwrap_or_else(|| "Not permitted".to_string()),
            InlineStatusTone::Warning,
        ),
        UserCardFetchState::AuthRequired => (
            status
                .message
                .clone()
                .unwrap_or_else(|| "Authentication required".to_string()),
            InlineStatusTone::Warning,
        ),
        UserCardFetchState::RetryableError if has_cached_value => (
            "Showing cached data while retry is available".to_string(),
            InlineStatusTone::Warning,
        ),
        UserCardFetchState::RetryableError => (
            status
                .message
                .clone()
                .unwrap_or_else(|| "Retry available".to_string()),
            InlineStatusTone::Warning,
        ),
        UserCardFetchState::Error if has_cached_value => (
            "Showing cached data after a refresh failure".to_string(),
            InlineStatusTone::Warning,
        ),
        UserCardFetchState::Error => (
            status
                .message
                .clone()
                .unwrap_or_else(|| "Could not load data".to_string()),
            InlineStatusTone::Warning,
        ),
        UserCardFetchState::Ready => return None,
    };

    Some(MetadataStatusRow {
        label,
        message,
        tone,
    })
}

fn render_metadata_status_row(row: MetadataStatusRow) -> impl IntoElement {
    let (bg, border, fg) = match row.tone {
        InlineStatusTone::Neutral => (
            colors::bg_surface(),
            colors::border(),
            colors::text_secondary(),
        ),
        InlineStatusTone::Success => (
            colors::accent().opacity(0.08),
            colors::accent().opacity(0.18),
            colors::accent(),
        ),
        InlineStatusTone::Warning => (
            colors::warning().opacity(0.08),
            colors::warning().opacity(0.18),
            colors::warning(),
        ),
    };

    div()
        .px_2()
        .py_1p5()
        .rounded(Radii::sm())
        .bg(bg)
        .border_1()
        .border_color(border)
        .flex()
        .items_center()
        .justify_between()
        .gap_2()
        .child(
            div()
                .text_xs()
                .text_color(colors::text_muted())
                .child(row.label),
        )
        .child(
            div()
                .text_xs()
                .font_weight(FontWeight::MEDIUM)
                .text_color(fg)
                .overflow_hidden()
                .child(row.message),
        )
}

fn render_inline_status(message: &str, tone: InlineStatusTone) -> impl IntoElement + use<> {
    let (bg, fg) = match tone {
        InlineStatusTone::Neutral => (colors::bg_surface(), colors::text_secondary()),
        InlineStatusTone::Success => (colors::accent().opacity(0.1), colors::accent()),
        InlineStatusTone::Warning => (colors::warning().opacity(0.1), colors::warning()),
    };

    div()
        .px_2()
        .py_1p5()
        .rounded(Radii::sm())
        .bg(bg)
        .text_xs()
        .text_color(fg)
        .child(message.to_string())
}

fn truncate_note_input(note: &str) -> String {
    note.trim().chars().take(USER_CARD_NOTE_MAX_LEN).collect()
}

fn render_streamer_mode_placeholder(message: &'static str) -> impl IntoElement {
    div()
        .p_3()
        .rounded(Radii::sm())
        .bg(colors::bg_surface())
        .text_xs()
        .text_color(colors::text_secondary())
        .text_center()
        .child(message)
}

fn queue_confirm_open_link(url: String) {
    backend::push_ui_action(backend::UiAction::ConfirmOpenLink {
        url,
        mode: backend::ui_actions::LinkOpenMode::Configured,
    });
}

fn seven_tv_profile_url(cosmetics: Option<&ResolvedUserCosmetics>) -> Option<String> {
    cosmetics
        .map(|cosmetics| cosmetics.user_id.trim())
        .filter(|user_id| !user_id.is_empty())
        .map(|user_id| format!("https://7tv.app/users/{user_id}"))
}

fn cosmetic_display_name(
    fallback_display_name: &str,
    cosmetics: Option<&ResolvedUserCosmetics>,
) -> String {
    cosmetics
        .and_then(|cosmetics| cosmetics.paint.as_ref())
        .map(|paint| paint.transformed_display_name(fallback_display_name))
        .unwrap_or_else(|| fallback_display_name.to_string())
}

fn cosmetic_user_color(cosmetics: Option<&ResolvedUserCosmetics>, fallback_color: Hsla) -> Hsla {
    cosmetics
        .and_then(|cosmetics| cosmetics.paint.as_ref())
        .and_then(|paint| paint.representative_color())
        .map(rgba_to_hsla)
        .unwrap_or(fallback_color)
}

fn rgba_to_hsla(color: crate::backend::cosmetics_core::RgbaColor) -> Hsla {
    let mut parsed = parse_hex_color_opt(&color.to_hex_rgb()).unwrap_or(colors::accent());
    parsed.a = color.a as f32 / 255.0;
    parsed
}

fn render_user_card_avatar(
    avatar_url: Option<&str>,
    avatar_is_animated: bool,
    display_name: &str,
    user_color: Hsla,
) -> AnyElement {
    let avatar_url = avatar_url.filter(|_| !backend::streamer_mode::should_hide_avatars());
    let base = div()
        .size(px(56.0))
        .rounded(Radii::lg())
        .overflow_hidden()
        .bg(user_color.opacity(0.2))
        .flex()
        .items_center()
        .justify_center();

    if let Some(url) = avatar_url {
        base.child(render_cosmetic_image(
            url,
            56.0,
            ObjectFit::Cover,
            avatar_is_animated,
        ))
        .into_any_element()
    } else {
        base.child(
            div()
                .text_xl()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(user_color)
                .child(
                    display_name
                        .chars()
                        .next()
                        .unwrap_or('?')
                        .to_uppercase()
                        .to_string(),
                ),
        )
        .into_any_element()
    }
}

/// Render a badge image
fn render_badge(badge_key: &str, channel: &str) -> impl IntoElement + use<> {
    let url = backend::get_badge_url(badge_key, channel);
    let source_id = format!("user-card-badge-{}", badge_key.replace(['/', ':'], "-"));
    let tooltip_text = if backend::is_using_ffz_authority_badge(badge_key, channel) {
        let badge_name = backend::badges::badge_key_name(badge_key);
        format!("FFZ Channel {} Badge", humanize_badge_label(badge_name))
    } else {
        user_card_badge_tooltip_text(badge_key)
    };

    if let Some(url) = url {
        div()
            .id(source_id.clone())
            .size(px(18.0))
            .mr_0p5()
            .on_hover(text_tooltip_on_hover(
                source_id.clone(),
                tooltip_text.clone(),
            ))
            .child(img(url).size(px(18.0)).object_fit(ObjectFit::Contain))
            .into_any_element()
    } else {
        div()
            .id(source_id.clone())
            .size(px(18.0))
            .mr_0p5()
            .rounded(Radii::xs())
            .bg(colors::bg_elevated())
            .flex()
            .items_center()
            .justify_center()
            .text_xs()
            .on_hover(text_tooltip_on_hover(
                source_id.clone(),
                tooltip_text.clone(),
            ))
            .child(
                badge_key
                    .chars()
                    .next()
                    .unwrap_or('?')
                    .to_uppercase()
                    .to_string(),
            )
            .into_any_element()
    }
}

fn user_card_badge_tooltip_text(badge_key: &str) -> String {
    let (name, version) = badge_name_and_version(badge_key);

    match (name.as_str(), version) {
        ("subscriber", Some(months)) => format!("Subscriber Badge - {months} months"),
        ("founder", Some(months)) => format!("Founder Badge - {months} months"),
        ("bits", Some(amount)) => format!("Bits Badge - {amount}"),
        ("sub-gifter", Some(count)) => format!("Sub Gifter Badge - {count}"),
        ("ffz", Some(kind)) => format!("FFZ Badge - {}", humanize_badge_label(kind)),
        ("ffz-bot", Some(kind)) => {
            format!("FFZ Bot Badge - {}", humanize_badge_label(kind))
        }
        ("bttv", Some(kind)) => format!("BTTV Badge - {}", humanize_badge_label(kind)),
        ("bttv-pro", Some(kind)) | ("betterttv-pro", Some(kind)) => {
            format!("BTTV Pro Badge - {}", humanize_badge_label(kind))
        }
        _ => {
            if let Some(version) = version {
                format!(
                    "{} Badge - {}",
                    humanize_badge_label(&name),
                    humanize_badge_label(version)
                )
            } else {
                format!("{} Badge", humanize_badge_label(&name))
            }
        }
    }
}

fn humanize_badge_label(value: &str) -> String {
    value
        .split(['-', '_'])
        .filter(|part| !part.is_empty())
        .map(|part| {
            let mut chars = part.chars();
            match chars.next() {
                Some(first) => {
                    first.to_uppercase().collect::<String>() + &chars.as_str().to_ascii_lowercase()
                }
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn render_7tv_badge(badge: &SevenTvBadge) -> AnyElement {
    let source_id = format!("user-card-7tv-badge-{}", badge.id);
    let tooltip_text = seven_tv_badge_tooltip_text(badge);

    div()
        .id(source_id.clone())
        .size(px(18.0))
        .mr_0p5()
        .on_hover(text_tooltip_on_hover(
            source_id.clone(),
            tooltip_text.clone(),
        ))
        .child(render_cosmetic_image(
            &badge.url,
            18.0,
            ObjectFit::Contain,
            badge.is_animated,
        ))
        .into_any_element()
}

fn render_cosmetic_image(
    url: &str,
    size_px: f32,
    object_fit: ObjectFit,
    is_animated: bool,
) -> AnyElement {
    let size = px(size_px);
    if is_animated {
        if let Some(render_image) = image_cache::get_current_animated_render_image(url) {
            if let Some(metadata) = image_cache::get_animation_metadata(url) {
                crate::emote_renderer::register_visible_metadata(metadata);
            }
            return img(render_image)
                .size(size)
                .object_fit(object_fit)
                .into_any_element();
        }
        if let Some(render_image) = image_cache::get_cached_render_image(url) {
            return img(render_image)
                .size(size)
                .object_fit(object_fit)
                .into_any_element();
        }
        if matches!(
            image_cache::get_animated_cache_state(url),
            image_cache::AnimatedCacheState::NotLoaded
        ) {
            image_cache::trigger_animated_decode_with_source(
                url,
                image_cache::PreloadSource::VisibleLayout,
            );
        }
    }

    if is_animated {
        return img(SharedString::from(url))
            .id((
                "user-card-cosmetic-animation",
                stable_image_instance_id(url),
            ))
            .size(size)
            .object_fit(object_fit)
            .into_any_element();
    }
    img(SharedString::from(url))
        .size(size)
        .object_fit(object_fit)
        .into_any_element()
}

fn stable_image_instance_id(url: &str) -> u64 {
    use std::hash::{Hash, Hasher};

    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    url.hash(&mut hasher);
    hasher.finish()
}

fn seven_tv_badge_tooltip_text(badge: &SevenTvBadge) -> String {
    let mut parts = Vec::new();
    if !badge.name.trim().is_empty() {
        parts.push(badge.name.trim().to_string());
    }
    if !badge.tooltip.trim().is_empty() && badge.tooltip.trim() != badge.name.trim() {
        parts.push(badge.tooltip.trim().to_string());
    }
    if badge.is_animated {
        parts.push("Animated 7TV badge".to_string());
    } else {
        parts.push("7TV badge".to_string());
    }
    parts.join(" - ")
}

/// Render a message preview with timestamp
/// Height of inline emote/emoji images in user-card message rows.
const MESSAGE_ROW_EMOTE_HEIGHT: f32 = 20.0;

/// Resolved render inputs for one recent-message row, built once per context
/// change by `UserCard::ensure_recent_rows`.
struct RecentMessageRow {
    timestamp: Option<SharedString>,
    parts: Vec<RecentMessagePart>,
}

/// One resolved slot of a recent-message row.
enum RecentMessagePart {
    Text(SharedString),
    Url(SharedString),
    Mention {
        text: SharedString,
        color: Option<Hsla>,
    },
    Emote {
        emote: Arc<crate::backend::emotes::Emote>,
        url: SharedString,
        image_id: SharedString,
        /// A display rule shows the static first frame.
        still: bool,
    },
    Emoji {
        url: SharedString,
        image_id: SharedString,
    },
}

/// Render one recent message. The card header already establishes the user,
/// so rows retain their timestamp and authored content without repeating the
/// same sender name and badges for every buffered message.
fn render_rich_message_row(row: &RecentMessageRow) -> AnyElement {
    let mut content = div()
        .flex_1()
        .min_w_0()
        .flex()
        .flex_wrap()
        .items_center()
        .gap_x(px(Spacing::XS))
        .text_xs();

    for part in &row.parts {
        content = match part {
            RecentMessagePart::Text(text) => content.child(
                div()
                    .min_w_0()
                    .text_color(colors::text_primary())
                    .child(text.clone()),
            ),
            RecentMessagePart::Emote {
                emote,
                url,
                image_id,
                still,
            } => content.child(render_inline_emote(emote, url, image_id, *still)),
            RecentMessagePart::Url(url) => content.child(
                div()
                    .min_w_0()
                    .text_color(colors::accent_text())
                    .child(url.clone()),
            ),
            RecentMessagePart::Mention { text, color } => content.child(
                div()
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(color.unwrap_or_else(colors::accent))
                    .child(text.clone()),
            ),
            RecentMessagePart::Emoji { url, image_id } => content.child(render_inline_image(
                url,
                image_id,
                MESSAGE_ROW_EMOTE_HEIGHT * 0.85,
            )),
        };
    }

    div()
        .px_2()
        .py_1()
        .hover(|s| s.bg(colors::bg_hover()))
        .flex()
        .items_start()
        .gap_2()
        .when_some(row.timestamp.clone(), |element, timestamp| {
            element.child(
                div()
                    .w(px(52.0))
                    .flex_none()
                    .text_xs()
                    .text_color(colors::text_muted())
                    .child(timestamp),
            )
        })
        .child(content)
        .into_any_element()
}

/// Render an emote inline, preferring already-decoded frames from the shared
/// cache (instant; the same images chat just painted) and falling back to
/// GPUI's async image loading for anything not cached yet. A `still` emote uses
/// only Echo's static first frame, never an animated decode or `img(url)`.
fn render_inline_emote(
    emote: &Arc<crate::backend::emotes::Emote>,
    url: &SharedString,
    image_id: &SharedString,
    still: bool,
) -> AnyElement {
    let url_str: &str = url.as_ref();

    if still {
        if let Some(render_image) = image_cache::get_cached_render_image(url_str) {
            return render_image_element_sized(render_image, MESSAGE_ROW_EMOTE_HEIGHT);
        }
        return img(crate::components::ui::still_frame_source(
            url.clone(),
            image_cache::PreloadSource::VisibleChat,
        ))
        .id(ElementId::Name(image_id.clone()))
        .h(px(MESSAGE_ROW_EMOTE_HEIGHT))
        .min_w(px(MESSAGE_ROW_EMOTE_HEIGHT))
        .object_fit(ObjectFit::Contain)
        .into_any_element();
    }

    if emote.is_animated {
        if let Some(render_image) = image_cache::get_current_animated_render_image(url_str) {
            return render_image_element_sized(render_image, MESSAGE_ROW_EMOTE_HEIGHT);
        }
        if matches!(
            image_cache::get_animated_cache_state(url_str),
            image_cache::AnimatedCacheState::NotLoaded
        ) {
            image_cache::trigger_animated_decode_with_source(
                url_str,
                image_cache::PreloadSource::VisibleChat,
            );
        }
    }
    if let Some(render_image) = image_cache::get_cached_render_image(url_str) {
        return render_image_element_sized(render_image, MESSAGE_ROW_EMOTE_HEIGHT);
    }

    render_inline_image(url, image_id, MESSAGE_ROW_EMOTE_HEIGHT)
}

/// Decoded image scaled to a row height, preserving aspect ratio.
fn render_image_element_sized(render_image: Arc<RenderImage>, height: f32) -> AnyElement {
    let image_size = render_image.size(0);
    let width: f32 = image_size.width.0 as f32;
    let image_height: f32 = image_size.height.0 as f32;
    let aspect = if image_height > 0.0 {
        width / image_height
    } else {
        1.0
    };

    img(ImageSource::Render(render_image))
        .h(px(height))
        .w(px(height * aspect))
        .object_fit(ObjectFit::Contain)
        .into_any_element()
}

/// URI-loaded inline image with a stable id (GPUI fetches asynchronously).
fn render_inline_image(url: &SharedString, image_id: &SharedString, height: f32) -> AnyElement {
    img(url.clone())
        .id(ElementId::Name(image_id.clone()))
        .h(px(height))
        .min_w(px(height))
        .object_fit(ObjectFit::Contain)
        .into_any_element()
}

fn subscription_summary_from_badges(badges: &[String]) -> Option<String> {
    let mut subscriber_months = None;
    let mut has_founder = false;
    let mut has_bttv_pro = false;

    for badge in badges {
        let (name, version) = badge_name_and_version(badge);
        match name.as_str() {
            "subscriber" => {
                subscriber_months = version.and_then(|value| value.parse::<u32>().ok());
            }
            "founder" => {
                has_founder = true;
            }
            "bttv-pro" | "betterttv-pro" if version == Some("subscriber") => {
                has_bttv_pro = true;
            }
            _ => {}
        }
    }

    let mut parts = Vec::new();
    match (subscriber_months, has_founder) {
        (Some(1), true) => parts.push("Founder - 1 month".to_string()),
        (Some(months), true) => parts.push(format!("Founder - {months} months")),
        (Some(1), false) => parts.push("1 month".to_string()),
        (Some(months), false) => parts.push(format!("{months} months")),
        (None, true) => parts.push("Founder".to_string()),
        (None, false) => {}
    }
    if has_bttv_pro {
        parts.push("BTTV Pro Subscriber".to_string());
    }

    (!parts.is_empty()).then(|| parts.join(" - "))
}

fn subscription_summary_from_live_and_badges(
    subscription_info: Option<&backend::twitch_api::TwitchUserSubscriptionInfo>,
    badges: &[String],
) -> Option<String> {
    let badge_summary = subscription_summary_from_badges(badges);
    let Some(info) = subscription_info.filter(|info| info.is_subscribed) else {
        return badge_summary;
    };

    let mut parts = Vec::new();
    parts.push(
        format_subscription_tier(info.tier.as_deref())
            .or_else(|| info.plan_name.clone())
            .unwrap_or_else(|| "Subscribed".to_string()),
    );

    if let Some(months) = badge_summary
        && !parts.iter().any(|part| part == &months)
    {
        parts.push(months);
    }

    if info.is_gift {
        let gifter = info
            .gifter_name
            .as_deref()
            .or(info.gifter_login.as_deref())
            .map(str::trim)
            .filter(|name| !name.is_empty());
        match gifter {
            Some(name) => parts.push(format!("gifted by {name}")),
            None => parts.push("gifted".to_string()),
        }
    }

    Some(parts.join(" - "))
}

fn format_subscription_tier(tier: Option<&str>) -> Option<String> {
    match tier.map(str::trim).filter(|tier| !tier.is_empty())? {
        "1000" => Some("Tier 1".to_string()),
        "2000" => Some("Tier 2".to_string()),
        "3000" => Some("Tier 3".to_string()),
        other => Some(other.to_string()),
    }
}

fn badge_name_and_version(badge: &str) -> (String, Option<&str>) {
    let badge = badge.trim();
    if let Some((name, version)) = badge.split_once('/') {
        return (name.to_ascii_lowercase(), Some(version));
    }
    if let Some((name, version)) = badge.split_once(':') {
        return (name.to_ascii_lowercase(), Some(version));
    }
    (badge.to_ascii_lowercase(), None)
}
