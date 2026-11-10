//! Types the UI reads from the backend through `bridge`. Some re-export the
//! backend's own types; the others are UI-facing copies with `From`
//! conversions, mostly of settings types.

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

    Some(
        crate::cache::disk_layer::MediaSourceId::with_interned_provider(
            provider,
            std::sync::Arc::clone(&emote.id),
            variant,
        ),
    )
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

/// IRC connection status.
pub use crate::backend::irc::ConnectionStatus;

/// Authentication state
pub use crate::backend::auth::AuthState;

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

pub use crate::backend::settings::{BadgeDisplayMode, TabOverflowMode, TabTheme};

/// Application settings
#[derive(Clone, Debug)]
pub struct AppSettings {
    pub tab_theme: TabTheme,
    pub anonymous_mode: bool,
}

impl From<crate::backend::settings::AppSettings> for AppSettings {
    fn from(s: crate::backend::settings::AppSettings) -> Self {
        AppSettings {
            tab_theme: s.tab_theme,
            anonymous_mode: s.anonymous_mode,
        }
    }
}

pub use echo_emote_core::EmoteCategoryView as EmoteCategory;

/// Live stream information for streamer bar
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
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

/// Twitch EventSub events and their kinds.
pub use crate::backend::components::event_notification::{
    PredictionOutcomeSnapshot, TwitchEvent, TwitchEventType,
};

/// Moderation log entries and their kinds.
pub use crate::backend::components::mod_log::{ModAction, ModActionType};

pub use crate::backend::sounds::SoundEventType;

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

pub use crate::backend::settings::{FilterAction, FilterRule};

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
