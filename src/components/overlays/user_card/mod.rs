//! User card: a chatter's profile, badges, local note, recent messages and
//! moderation row, opened from a username click.
//!
//! The card renders either as a draggable overlay or as the root of its own
//! popout window (`hosted_in_window`). This module owns the state, metadata
//! fetching and the refresh/animation wakers; `view` builds and paints each
//! frame, `note`, `recent_messages` and `mod_actions` own their sections, and
//! `badges` renders header badges and cosmetics.

mod badges;
mod mod_actions;
mod note;
mod recent_messages;
mod view;

use super::ConfirmationModal;
use super::traits::Openable;
use crate::backend;
use crate::backend::irc::resolve_user_color;
use crate::cache::loader as image_cache;
use crate::components::ui::input::InputState;
use crate::theme::colors;
use crate::utils::parse_user_color_opt;
use gpui::*;
use note::NoteEditor;
use recent_messages::RecentMessageRow;
use std::sync::Arc;
use std::time::Duration;

const USER_CARD_METADATA_FALLBACK_INTERVAL_MS: u64 = 1_000;
const USER_CARD_METADATA_FALLBACK_TICKS: usize = 12;

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
    note: NoteEditor,
    /// Reason field for Warn; Twitch shows the reason to the warned user, so
    /// the card never sends a canned one.
    warn_input: Option<Entity<InputState>>,
    warn_prompt_open: bool,
    subscriptions: Vec<Subscription>,
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
            note: NoteEditor::default(),
            warn_input: None,
            warn_prompt_open: false,
            subscriptions: Vec::new(),
            is_dragging: false,
            drag_offset: Point::default(),
            hosted_in_window: false,
            host_focus: None,
        };
        card.queue_metadata_fetches();
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
        self.queue_metadata_fetches();
        self.metadata_snapshot = self.capture_metadata_snapshot();
        self.note.unbind();
        self.warn_prompt_open = false;
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

    /// Queue every background lookup the card shows. Each queue call is
    /// deduplicated downstream, so the refresh loop may repeat this freely.
    fn queue_metadata_fetches(&self) {
        let state = &self.state;
        let user_context = &self.user_context;
        Self::queue_user_card_cosmetics(state, user_context);
        Self::queue_user_card_pronouns(state);
        Self::queue_user_card_twitch_profile(state);
        Self::queue_user_card_live_status(state);
        Self::queue_user_card_follow(state, user_context);
        Self::queue_user_card_subscription(state, user_context);
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
        let (Some(channel), Some(username)) = (state.channel.as_ref(), state.username.as_ref())
        else {
            return;
        };
        if !Self::shows_follow_metadata(
            channel,
            backend::streamer_mode::should_hide_user_card_details(),
        ) {
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
        let (Some(channel), Some(username)) = (state.channel.as_ref(), state.username.as_ref())
        else {
            return;
        };
        if !Self::shows_subscription_metadata(
            channel,
            username,
            backend::streamer_mode::should_hide_user_card_details(),
        ) {
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

    /// Whether the card fetches and shows follow metadata. Follower counts are
    /// public; the follow date needs moderator access to the channel.
    fn shows_follow_metadata(channel: &str, hide_user_details: bool) -> bool {
        !hide_user_details
            && (backend::settings::get_show_follower_count()
                || (backend::settings::get_show_follow_date()
                    && backend::irc::can_moderate_channel(channel)))
    }

    /// Whether the card fetches and shows subscription metadata, which Twitch
    /// only returns to the broadcaster or to the subscriber themselves.
    fn shows_subscription_metadata(channel: &str, username: &str, hide_user_details: bool) -> bool {
        let viewer = backend::settings::get_username_lowercase();
        !hide_user_details
            && !viewer.is_empty()
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

    /// Point the animation waker at the animated avatar, the animated 7TV
    /// badge and the recent rows' animated emotes, restarting it if it stopped.
    fn retarget_animation_waker(
        &mut self,
        avatar_url: Option<String>,
        badge_url: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let animation_urls_key = AnimationUrlsKey {
            rows_generation: self.recent_rows_generation,
            avatar_url,
            badge_url,
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
                        card.queue_metadata_fetches();

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

    /// Move the card with the pointer, ending the drag once the left button
    /// is no longer held.
    fn apply_drag(&mut self, event: &MouseMoveEvent, cx: &mut Context<Self>) {
        if event.pressed_button != Some(MouseButton::Left) {
            self.is_dragging = false;
            cx.notify();
            return;
        }
        let new_x: f32 = event.position.x.into();
        let new_y: f32 = event.position.y.into();
        let offset_x: f32 = self.drag_offset.x.into();
        let offset_y: f32 = self.drag_offset.y.into();
        self.state.x = new_x - offset_x;
        self.state.y = new_y - offset_y;
        cx.notify();
    }
}
