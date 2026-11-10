//! Backend types for Echo
//!
//! These types provide a clean API for the GPUI frontend.
//! They mirror the internal logic types but are designed for external use.
#![allow(clippy::upper_case_acronyms)]

use serde::{Deserialize, Serialize};

pub use echo_emote_core::{EmoteProvider, EmoteView as Emote};

pub fn media_source_id_for_url(
    emote: &Emote,
    url: &str,
) -> Option<crate::cache::disk_layer::MediaSourceId> {
    if emote.id.is_empty() || url.is_empty() {
        return None;
    }

    let provider = emote.provider.stable_cache_key();
    let variant = crate::cache::disk_layer::MediaVariant::for_emote_url(
        provider,
        emote.source_scale(url),
        url,
        emote.is_animated,
    );

    Some(crate::cache::disk_layer::MediaSourceId::new_shared_labels(
        provider,
        std::sync::Arc::clone(&emote.id),
        variant,
    ))
}

pub fn media_request_for_url(emote: &Emote, url: &str) -> crate::cache::loader::MediaRequest {
    let shared_url = emote.url_shared(url);
    crate::cache::loader::MediaRequest::new(shared_url, media_source_id_for_url(emote, url))
}

/// Re-export HighlightType from models module (single source of truth)
pub use crate::backend::models::HighlightType;

/// Re-export ReplyInfo from models module (single source of truth)
pub use crate::backend::models::ReplyInfo;

/// Re-export ChatMessage from models module (single source of truth)
pub use crate::backend::models::ChatMessage;

/// IRC connection status
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub enum ConnectionStatus {
    Connected,
    #[default]
    Reconnecting,
    Disconnected,
}

impl From<crate::backend::irc::ConnectionStatus> for ConnectionStatus {
    fn from(s: crate::backend::irc::ConnectionStatus) -> Self {
        match s {
            crate::backend::irc::ConnectionStatus::Connected => ConnectionStatus::Connected,
            crate::backend::irc::ConnectionStatus::Reconnecting => ConnectionStatus::Reconnecting,
            crate::backend::irc::ConnectionStatus::Disconnected => ConnectionStatus::Disconnected,
        }
    }
}

/// Authentication state
#[derive(Clone, Debug, Default)]
pub struct AuthState {
    pub authenticated: bool,
    pub username: Option<String>,
    pub display_name: Option<String>,
    pub profile_image_url: Option<String>,
    pub device_code: Option<String>,
    pub verification_uri: Option<String>,
    pub user_code: Option<String>,
    pub error: Option<String>,
    pub loading: bool,
    pub needs_reauth: bool,
}

impl From<crate::backend::auth::AuthState> for AuthState {
    fn from(a: crate::backend::auth::AuthState) -> Self {
        AuthState {
            authenticated: a.authenticated,
            username: a.username,
            display_name: a.display_name,
            profile_image_url: a.profile_image_url,
            device_code: a.device_code,
            verification_uri: a.verification_uri,
            user_code: a.user_code,
            error: a.error,
            loading: a.loading,
            needs_reauth: a.needs_reauth,
        }
    }
}

/// Account data for multi-account support
#[derive(Clone, Debug)]
pub struct Account {
    pub username: String,
    pub display_name: Option<String>,
    pub profile_image_url: Option<String>,
    pub is_active: bool,
}

impl From<crate::backend::settings::Account> for Account {
    fn from(a: crate::backend::settings::Account) -> Self {
        Account {
            username: a.username,
            display_name: a.display_name,
            profile_image_url: a.profile_image_url,
            is_active: a.is_active,
        }
    }
}

/// Tab theme options
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub enum TabTheme {
    #[default]
    Chrome,
    Firefox,
    Edge,
    Square,
    Arc,
    Minimal,
}

impl From<crate::backend::settings::TabTheme> for TabTheme {
    fn from(t: crate::backend::settings::TabTheme) -> Self {
        match t {
            crate::backend::settings::TabTheme::Chrome => TabTheme::Chrome,
            crate::backend::settings::TabTheme::Firefox => TabTheme::Firefox,
            crate::backend::settings::TabTheme::Edge => TabTheme::Edge,
            crate::backend::settings::TabTheme::Square => TabTheme::Square,
            crate::backend::settings::TabTheme::Arc => TabTheme::Arc,
            crate::backend::settings::TabTheme::Minimal => TabTheme::Minimal,
        }
    }
}

impl From<TabTheme> for crate::backend::settings::TabTheme {
    fn from(t: TabTheme) -> Self {
        match t {
            TabTheme::Chrome => crate::backend::settings::TabTheme::Chrome,
            TabTheme::Firefox => crate::backend::settings::TabTheme::Firefox,
            TabTheme::Edge => crate::backend::settings::TabTheme::Edge,
            TabTheme::Square => crate::backend::settings::TabTheme::Square,
            TabTheme::Arc => crate::backend::settings::TabTheme::Arc,
            TabTheme::Minimal => crate::backend::settings::TabTheme::Minimal,
        }
    }
}

/// Tab overflow mode options - how tabs behave when they don't fit
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub enum TabOverflowMode {
    MultiLine,
    Dropdown,
    #[default]
    Scroll,
}

impl From<crate::backend::settings::TabOverflowMode> for TabOverflowMode {
    fn from(m: crate::backend::settings::TabOverflowMode) -> Self {
        match m {
            crate::backend::settings::TabOverflowMode::MultiLine => TabOverflowMode::MultiLine,
            crate::backend::settings::TabOverflowMode::Dropdown => TabOverflowMode::Dropdown,
            crate::backend::settings::TabOverflowMode::Scroll => TabOverflowMode::Scroll,
        }
    }
}

impl From<TabOverflowMode> for crate::backend::settings::TabOverflowMode {
    fn from(m: TabOverflowMode) -> Self {
        match m {
            TabOverflowMode::MultiLine => crate::backend::settings::TabOverflowMode::MultiLine,
            TabOverflowMode::Dropdown => crate::backend::settings::TabOverflowMode::Dropdown,
            TabOverflowMode::Scroll => crate::backend::settings::TabOverflowMode::Scroll,
        }
    }
}

/// Badge display mode options
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub enum BadgeDisplayMode {
    #[default]
    Number,
    Dot,
    Highlight,
}

impl From<crate::backend::settings::BadgeDisplayMode> for BadgeDisplayMode {
    fn from(m: crate::backend::settings::BadgeDisplayMode) -> Self {
        match m {
            crate::backend::settings::BadgeDisplayMode::Number => BadgeDisplayMode::Number,
            crate::backend::settings::BadgeDisplayMode::Dot => BadgeDisplayMode::Dot,
            crate::backend::settings::BadgeDisplayMode::Highlight => BadgeDisplayMode::Highlight,
        }
    }
}

impl From<BadgeDisplayMode> for crate::backend::settings::BadgeDisplayMode {
    fn from(m: BadgeDisplayMode) -> Self {
        match m {
            BadgeDisplayMode::Number => crate::backend::settings::BadgeDisplayMode::Number,
            BadgeDisplayMode::Dot => crate::backend::settings::BadgeDisplayMode::Dot,
            BadgeDisplayMode::Highlight => crate::backend::settings::BadgeDisplayMode::Highlight,
        }
    }
}

/// Application settings
#[derive(Clone, Debug)]
pub struct AppSettings {
    pub tab_theme: TabTheme,
    pub anonymous_mode: bool,
}

impl From<crate::backend::settings::AppSettings> for AppSettings {
    fn from(s: crate::backend::settings::AppSettings) -> Self {
        AppSettings {
            tab_theme: s.tab_theme.into(),
            anonymous_mode: s.anonymous_mode,
        }
    }
}

pub use echo_emote_core::EmoteCategoryView as EmoteCategory;

/// Live stream information for streamer bar
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StreamInfo {
    pub title: String,
    pub game_name: Option<String>,
    pub viewer_count: u32,
    pub started_at: i64,
    pub is_live: bool,
}

/// Whisper thread summary for sidebar
#[derive(Clone, Debug)]
pub struct WhisperThread {
    pub username: String,
    pub last_message: String,
    pub unread_count: u32,
    pub last_timestamp: i64,
}

/// Twitch event types for notifications
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TwitchEventType {
    Subscribe,
    Resubscribe,
    GiftSub,
    Raid,
    ChannelPointRedemption,
    StreamOnline,
    StreamOffline,
    Ban,
    Unban,
    AutomodMessageHold,
    AutomodMessageUpdate,
    SuspiciousUserMessage,
    SuspiciousUserUpdate,
    PollBegin,
    PollProgress,
    PollEnd,
    PredictionBegin,
    PredictionProgress,
    PredictionLock,
    PredictionEnd,
    HypeTrainBegin,
    HypeTrainProgress,
    HypeTrainEnd,
    VipAdd,
    VipRemove,
    ModeratorAdd,
    ModeratorRemove,
    WarningAcknowledge,
    WarningSend,
    ShieldModeBegin,
    ShieldModeEnd,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PredictionOutcomeSnapshot {
    pub id: String,
    pub title: String,
    pub users: Option<u64>,
    pub channel_points: Option<u64>,
}

impl From<crate::backend::components::event_notification::PredictionOutcomeSnapshot>
    for PredictionOutcomeSnapshot
{
    fn from(
        outcome: crate::backend::components::event_notification::PredictionOutcomeSnapshot,
    ) -> Self {
        PredictionOutcomeSnapshot {
            id: outcome.id,
            title: outcome.title,
            users: outcome.users,
            channel_points: outcome.channel_points,
        }
    }
}

impl From<crate::backend::components::event_notification::TwitchEventType> for TwitchEventType {
    fn from(t: crate::backend::components::event_notification::TwitchEventType) -> Self {
        use crate::backend::components::event_notification::TwitchEventType as T;
        match t {
            T::Subscribe => TwitchEventType::Subscribe,
            T::Resubscribe => TwitchEventType::Resubscribe,
            T::GiftSub => TwitchEventType::GiftSub,
            T::Raid => TwitchEventType::Raid,
            T::ChannelPointRedemption => TwitchEventType::ChannelPointRedemption,
            T::StreamOnline => TwitchEventType::StreamOnline,
            T::StreamOffline => TwitchEventType::StreamOffline,
            T::Ban => TwitchEventType::Ban,
            T::Unban => TwitchEventType::Unban,
            T::AutomodMessageHold => TwitchEventType::AutomodMessageHold,
            T::AutomodMessageUpdate => TwitchEventType::AutomodMessageUpdate,
            T::SuspiciousUserMessage => TwitchEventType::SuspiciousUserMessage,
            T::SuspiciousUserUpdate => TwitchEventType::SuspiciousUserUpdate,
            T::PollBegin => TwitchEventType::PollBegin,
            T::PollProgress => TwitchEventType::PollProgress,
            T::PollEnd => TwitchEventType::PollEnd,
            T::PredictionBegin => TwitchEventType::PredictionBegin,
            T::PredictionProgress => TwitchEventType::PredictionProgress,
            T::PredictionLock => TwitchEventType::PredictionLock,
            T::PredictionEnd => TwitchEventType::PredictionEnd,
            T::HypeTrainBegin => TwitchEventType::HypeTrainBegin,
            T::HypeTrainProgress => TwitchEventType::HypeTrainProgress,
            T::HypeTrainEnd => TwitchEventType::HypeTrainEnd,
            T::VipAdd => TwitchEventType::VipAdd,
            T::VipRemove => TwitchEventType::VipRemove,
            T::ModeratorAdd => TwitchEventType::ModeratorAdd,
            T::ModeratorRemove => TwitchEventType::ModeratorRemove,
            T::WarningAcknowledge => TwitchEventType::WarningAcknowledge,
            T::WarningSend => TwitchEventType::WarningSend,
            T::ShieldModeBegin => TwitchEventType::ShieldModeBegin,
            T::ShieldModeEnd => TwitchEventType::ShieldModeEnd,
        }
    }
}

/// Twitch event data for notifications
#[derive(Clone, Debug)]
pub struct TwitchEvent {
    pub event_type: TwitchEventType,
    pub channel: String,
    pub timestamp: i64,
    pub user_name: Option<String>,
    pub user_login: Option<String>,
    pub user_id: Option<String>,
    pub tier: Option<String>,
    pub cumulative_months: Option<u32>,
    pub message: Option<String>,
    pub total: Option<u32>,
    pub from_channel: Option<String>,
    pub viewers: Option<u32>,
    pub reward_id: Option<String>,
    pub reward_title: Option<String>,
    pub reward_cost: Option<u32>,
    pub user_input: Option<String>,
    pub message_id: Option<String>,
    pub moderator_name: Option<String>,
    pub reason: Option<String>,
    pub status: Option<String>,
    pub event_title: Option<String>,
    pub event_detail: Option<String>,
    pub is_permanent: bool,
}

impl From<crate::backend::components::event_notification::TwitchEvent> for TwitchEvent {
    fn from(e: crate::backend::components::event_notification::TwitchEvent) -> Self {
        TwitchEvent {
            event_type: e.event_type.into(),
            channel: e.channel,
            timestamp: e.timestamp,
            user_name: e.user_name,
            user_login: e.user_login,
            user_id: e.user_id,
            tier: e.tier,
            cumulative_months: e.cumulative_months,
            message: e.message,
            total: e.total,
            from_channel: e.from_channel,
            viewers: e.viewers,
            reward_id: e.reward_id,
            reward_title: e.reward_title,
            reward_cost: e.reward_cost,
            user_input: e.user_input,
            message_id: e.message_id,
            moderator_name: e.moderator_name,
            reason: e.reason,
            status: e.status,
            event_title: e.event_title,
            event_detail: e.event_detail,
            is_permanent: e.is_permanent,
        }
    }
}

/// Mod action types
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModActionType {
    Ban,
    Unban,
    Timeout,
    Delete,
    Clear,
    Mod,
    Unmod,
    Vip,
    Unvip,
    Warning,
    ShieldEnable,
    ShieldDisable,
}

impl From<crate::backend::components::mod_log::ModActionType> for ModActionType {
    fn from(t: crate::backend::components::mod_log::ModActionType) -> Self {
        match t {
            crate::backend::components::mod_log::ModActionType::Ban => ModActionType::Ban,
            crate::backend::components::mod_log::ModActionType::Unban => ModActionType::Unban,
            crate::backend::components::mod_log::ModActionType::Timeout => ModActionType::Timeout,
            crate::backend::components::mod_log::ModActionType::Delete => ModActionType::Delete,
            crate::backend::components::mod_log::ModActionType::Clear => ModActionType::Clear,
            crate::backend::components::mod_log::ModActionType::Mod => ModActionType::Mod,
            crate::backend::components::mod_log::ModActionType::Unmod => ModActionType::Unmod,
            crate::backend::components::mod_log::ModActionType::Vip => ModActionType::Vip,
            crate::backend::components::mod_log::ModActionType::Unvip => ModActionType::Unvip,
            crate::backend::components::mod_log::ModActionType::Warning => ModActionType::Warning,
            crate::backend::components::mod_log::ModActionType::ShieldEnable => {
                ModActionType::ShieldEnable
            }
            crate::backend::components::mod_log::ModActionType::ShieldDisable => {
                ModActionType::ShieldDisable
            }
        }
    }
}

/// Mod action log entry
#[derive(Clone, Debug)]
pub struct ModAction {
    pub action_type: ModActionType,
    pub username: String,
    pub channel: String,
    pub details: String,
    pub timestamp: i64,
}

impl From<crate::backend::components::mod_log::ModAction> for ModAction {
    fn from(a: crate::backend::components::mod_log::ModAction) -> Self {
        ModAction {
            action_type: a.action_type.into(),
            username: a.username,
            channel: a.channel,
            details: a.details,
            timestamp: a.timestamp,
        }
    }
}

/// Sound event types
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum SoundEventType {
    Mention,
    Whisper,
    HighlightedMessage,
    FirstMessage,
    ReturningChatter,
    Subscription,
    Raid,
    BitsBadge,
    Announcement,
    Phrase,
    ChannelPointRedeem,
    StreamOnline,
    StreamOffline,
}

impl From<crate::backend::sounds::SoundEventType> for SoundEventType {
    fn from(t: crate::backend::sounds::SoundEventType) -> Self {
        match t {
            crate::backend::sounds::SoundEventType::Mention => SoundEventType::Mention,
            crate::backend::sounds::SoundEventType::Whisper => SoundEventType::Whisper,
            crate::backend::sounds::SoundEventType::HighlightedMessage => {
                SoundEventType::HighlightedMessage
            }
            crate::backend::sounds::SoundEventType::FirstMessage => SoundEventType::FirstMessage,
            crate::backend::sounds::SoundEventType::ReturningChatter => {
                SoundEventType::ReturningChatter
            }
            crate::backend::sounds::SoundEventType::Subscription => SoundEventType::Subscription,
            crate::backend::sounds::SoundEventType::Raid => SoundEventType::Raid,
            crate::backend::sounds::SoundEventType::BitsBadge => SoundEventType::BitsBadge,
            crate::backend::sounds::SoundEventType::Announcement => SoundEventType::Announcement,
            crate::backend::sounds::SoundEventType::Phrase => SoundEventType::Phrase,
            crate::backend::sounds::SoundEventType::ChannelPointRedeem => {
                SoundEventType::ChannelPointRedeem
            }
            crate::backend::sounds::SoundEventType::StreamOnline => SoundEventType::StreamOnline,
            crate::backend::sounds::SoundEventType::StreamOffline => SoundEventType::StreamOffline,
        }
    }
}

impl From<SoundEventType> for crate::backend::sounds::SoundEventType {
    fn from(t: SoundEventType) -> Self {
        match t {
            SoundEventType::Mention => crate::backend::sounds::SoundEventType::Mention,
            SoundEventType::Whisper => crate::backend::sounds::SoundEventType::Whisper,
            SoundEventType::HighlightedMessage => {
                crate::backend::sounds::SoundEventType::HighlightedMessage
            }
            SoundEventType::FirstMessage => crate::backend::sounds::SoundEventType::FirstMessage,
            SoundEventType::ReturningChatter => {
                crate::backend::sounds::SoundEventType::ReturningChatter
            }
            SoundEventType::Subscription => crate::backend::sounds::SoundEventType::Subscription,
            SoundEventType::Raid => crate::backend::sounds::SoundEventType::Raid,
            SoundEventType::BitsBadge => crate::backend::sounds::SoundEventType::BitsBadge,
            SoundEventType::Announcement => crate::backend::sounds::SoundEventType::Announcement,
            SoundEventType::Phrase => crate::backend::sounds::SoundEventType::Phrase,
            SoundEventType::ChannelPointRedeem => {
                crate::backend::sounds::SoundEventType::ChannelPointRedeem
            }
            SoundEventType::StreamOnline => crate::backend::sounds::SoundEventType::StreamOnline,
            SoundEventType::StreamOffline => crate::backend::sounds::SoundEventType::StreamOffline,
        }
    }
}

/// Sound event settings
#[derive(Clone, Debug)]
pub struct SoundEventSettings {
    pub event_type: SoundEventType,
    pub display_name: String,
    pub enabled: bool,
    pub volume: f32,
}

/// Sound settings
#[derive(Clone, Debug)]
pub struct SoundSettings {
    pub global_enabled: bool,
    pub global_volume: f32,
    pub desktop_notifications_enabled: bool,
    pub muted_channels: Vec<String>,
    pub events: Vec<SoundEventSettings>,
}

/// Filter action types
#[derive(Clone, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum FilterAction {
    #[default]
    Allow,
    Hide,
    Highlight,
    Replace,
}

impl From<crate::backend::settings::FilterAction> for FilterAction {
    fn from(a: crate::backend::settings::FilterAction) -> Self {
        match a {
            crate::backend::settings::FilterAction::Allow => FilterAction::Allow,
            crate::backend::settings::FilterAction::Hide => FilterAction::Hide,
            crate::backend::settings::FilterAction::Highlight => FilterAction::Highlight,
            crate::backend::settings::FilterAction::Replace => FilterAction::Replace,
        }
    }
}

impl From<FilterAction> for crate::backend::settings::FilterAction {
    fn from(a: FilterAction) -> Self {
        match a {
            FilterAction::Allow => crate::backend::settings::FilterAction::Allow,
            FilterAction::Hide => crate::backend::settings::FilterAction::Hide,
            FilterAction::Highlight => crate::backend::settings::FilterAction::Highlight,
            FilterAction::Replace => crate::backend::settings::FilterAction::Replace,
        }
    }
}

/// Filter rule for message filtering
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FilterRule {
    pub id: String,
    pub expression: String,
    pub action: FilterAction,
    pub enabled: bool,
    #[serde(default)]
    pub replacement: Option<String>,
    #[serde(default)]
    pub pattern_is_regex: bool,
}

impl From<crate::backend::settings::FilterRule> for FilterRule {
    fn from(r: crate::backend::settings::FilterRule) -> Self {
        FilterRule {
            id: r.id,
            expression: r.expression,
            action: r.action.into(),
            enabled: r.enabled,
            replacement: r.replacement,
            pattern_is_regex: r.pattern_is_regex,
        }
    }
}

impl From<FilterRule> for crate::backend::settings::FilterRule {
    fn from(r: FilterRule) -> Self {
        crate::backend::settings::FilterRule {
            id: r.id,
            expression: r.expression,
            action: r.action.into(),
            enabled: r.enabled,
            replacement: r.replacement,
            pattern_is_regex: r.pattern_is_regex,
        }
    }
}

/// Custom chat command
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CustomCommand {
    pub id: String,
    pub name: String,
    pub response: String,
    pub enabled: bool,
}

impl From<crate::backend::settings::CustomCommand> for CustomCommand {
    fn from(c: crate::backend::settings::CustomCommand) -> Self {
        CustomCommand {
            id: c.id,
            name: c.name,
            response: c.response,
            enabled: c.enabled,
        }
    }
}

impl From<CustomCommand> for crate::backend::settings::CustomCommand {
    fn from(c: CustomCommand) -> Self {
        crate::backend::settings::CustomCommand {
            id: c.id,
            name: c.name,
            response: c.response,
            enabled: c.enabled,
        }
    }
}

/// Extended local settings
#[derive(Clone, Debug)]
pub struct LocalSettings {
    pub mention_color: String,
    pub whisper_color: String,
    pub subscription_color: String,
    pub first_message_color: String,
    pub thread_highlight_color: String,
    pub flash_on_mention: bool,
    pub show_mention_in_title: bool,
    pub thread_highlight_enabled: bool,
    pub thread_highlight_sound_enabled: bool,
    pub thread_highlight_show_in_mentions: bool,
}
