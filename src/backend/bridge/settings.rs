//! Settings getters and setters
//!
//! Provides access to all app settings including theme, appearance, sounds,
//! highlights, upload, logging, proxy, filters, and custom commands.

use super::{
    AppSettings, BadgeDisplayMode, FilterRule, LocalSettings, RUNTIME, SoundEventSettings,
    SoundEventType, SoundSettings, TabOverflowMode, TabTheme,
};
use crate::backend::lock_utils::MutexExt;

/// Get all app settings
pub fn get_settings() -> Option<AppSettings> {
    let settings = crate::backend::settings::SETTINGS.lock_or_recover();
    Some(settings.clone().into())
}

/// Get local settings (highlights, filters, commands, etc.)
pub fn get_local_settings() -> LocalSettings {
    LocalSettings {
        mention_color: crate::backend::settings::get_mention_color(),
        whisper_color: crate::backend::settings::get_whisper_color(),
        subscription_color: crate::backend::settings::get_subscription_color(),
        first_message_color: crate::backend::settings::get_first_message_color(),
        thread_highlight_color: crate::backend::settings::get_thread_highlight_color(),
        flash_on_mention: crate::backend::settings::get_flash_on_mention(),
        show_mention_in_title: crate::backend::settings::get_show_mention_in_title(),
        thread_highlight_enabled: crate::backend::settings::get_thread_highlight_enabled(),
        thread_highlight_sound_enabled:
            crate::backend::settings::get_thread_highlight_sound_enabled(),
        thread_highlight_show_in_mentions:
            crate::backend::settings::get_thread_highlight_show_in_mentions(),
    }
}

/// The appearance fields the settings UI reads on every render.
#[derive(Clone, Debug)]
pub struct AppearanceSettings {
    pub theme: Option<String>,
    pub primary_accent_color: String,
    pub font_scale: f32,
    pub tab_theme: TabTheme,
    pub badge_display_mode: BadgeDisplayMode,
}

/// Read the appearance fields under a single lock.
///
/// `get_settings` clones the whole persisted settings struct; callers that only
/// need theme/accent/scale should use this instead.
pub fn get_appearance_settings() -> AppearanceSettings {
    let settings = crate::backend::settings::SETTINGS.lock_or_recover();
    AppearanceSettings {
        theme: settings.theme.clone(),
        primary_accent_color: settings.primary_accent_color.clone(),
        font_scale: settings.font_scale,
        tab_theme: settings.tab_theme.clone(),
        badge_display_mode: settings.badge_display_mode.clone(),
    }
}

/// Set theme
pub fn set_theme(theme: &str) {
    crate::backend::settings::set_theme(theme.to_string());
}

/// Set accent color
pub fn set_accent_color(color: &str) {
    crate::backend::settings::set_primary_accent_color(color.to_string());
}

/// Set font scale
pub fn set_font_scale(scale: f32) {
    crate::backend::settings::set_font_scale(scale);
}

/// Set tab theme style
pub fn set_tab_theme(theme: TabTheme) {
    crate::backend::settings::set_tab_theme(theme);
}

/// Get tab overflow mode
pub fn get_tab_overflow_mode() -> TabOverflowMode {
    crate::backend::settings::get_tab_overflow_mode()
}

/// Set tab overflow mode
pub fn set_tab_overflow_mode(mode: TabOverflowMode) {
    crate::backend::settings::set_tab_overflow_mode(mode);
}

/// Set badge display mode
pub fn set_badge_display_mode(mode: BadgeDisplayMode) {
    crate::backend::settings::set_badge_display_mode(mode);
}

/// Set anonymous mode
///
/// Keeps the persisted toggle, IRC runtime, and auth-backed caches in sync.
pub fn set_anonymous_mode(anonymous: bool) {
    let (blocked_users_auth_epoch, credential_generation) =
        crate::backend::auth::commit_runtime_mode_transition(anonymous);

    if anonymous {
        RUNTIME.spawn(async move {
            if !crate::backend::auth::clear_distributed_token_for_generation(credential_generation)
                .await
            {
                crate::log_auth!(debug, "Skipped stale anonymous-mode cache clear");
            }
        });
        return;
    }

    RUNTIME.spawn(async move {
        if let Err(error) = crate::backend::auth::restore_authenticated_runtime_after_anonymous(
            blocked_users_auth_epoch,
            credential_generation,
        )
        .await
        {
            crate::backend::push_ui_action(crate::backend::UiAction::ShowWarningToast {
                message: "Twitch account not restored".to_string(),
                detail: Some(error),
            });
        }
    });
}

/// Get sound settings
pub fn get_sound_settings() -> SoundSettings {
    let store = &crate::backend::sounds::SOUND_STORE;
    let settings = store.settings();

    let events: Vec<SoundEventSettings> = crate::backend::sounds::SoundEventType::all()
        .iter()
        .map(|event_type| {
            let event_settings = settings
                .events
                .get(event_type)
                .cloned()
                .unwrap_or_else(|| event_type.default_settings());
            SoundEventSettings {
                event_type: *event_type,
                display_name: event_type.display_name().to_string(),
                enabled: event_settings.enabled,
                volume: event_settings.volume,
            }
        })
        .collect();

    SoundSettings {
        global_enabled: settings.global_enabled,
        global_volume: settings.global_volume,
        desktop_notifications_enabled: settings.desktop_notifications_enabled,
        muted_channels: settings.muted_channels,
        events,
    }
}

/// Set global sound enabled
pub fn set_global_sound_enabled(enabled: bool) {
    crate::backend::sounds::SOUND_STORE.set_global_enabled(enabled);
}

/// Set global sound volume
pub fn set_global_sound_volume(volume: f32) {
    crate::backend::sounds::SOUND_STORE.set_global_volume(volume.clamp(0.0, 1.0));
}

/// Set desktop notifications enabled
pub fn set_desktop_notifications_enabled(enabled: bool) {
    crate::backend::sounds::SOUND_STORE.set_desktop_notifications_enabled(enabled);
}

/// Set sound event enabled
pub fn set_sound_event_enabled(event_type: SoundEventType, enabled: bool) {
    crate::backend::sounds::SOUND_STORE.set_event_enabled(event_type, enabled);
}

/// Set sound event volume
pub fn set_sound_event_volume(event_type: SoundEventType, volume: f32) {
    crate::backend::sounds::SOUND_STORE.set_event_volume(event_type, volume.clamp(0.0, 1.0));
}

/// Set per-channel sound and desktop notification mute state.
pub fn set_highlight_sound_channel_muted(channel: &str, muted: bool) -> bool {
    crate::backend::sounds::SOUND_STORE.set_channel_muted(channel, muted)
}

pub fn channel_desktop_notifications_muted(channel: &str) -> bool {
    crate::backend::sounds::SOUND_STORE.is_channel_desktop_muted(channel)
}

/// Clear all per-channel sound and desktop notification mutes.
pub fn clear_highlight_sound_muted_channels() {
    crate::backend::sounds::SOUND_STORE.clear_muted_channels();
}

/// Test sound playback
pub fn test_sound(event_type: SoundEventType) {
    crate::backend::sounds::SOUND_STORE.test_sound(event_type);
}

/// Set flash on mention
pub fn set_flash_on_mention(enabled: bool) {
    crate::backend::settings::set_flash_on_mention(enabled);
}

/// Set show mention in title
pub fn set_show_mention_in_title(enabled: bool) {
    crate::backend::settings::set_show_mention_in_title(enabled);
}

/// Set reply-thread highlight enablement
pub fn set_thread_highlight_enabled(enabled: bool) {
    crate::backend::settings::set_thread_highlight_enabled(enabled);
}

/// Set reply-thread highlight sound enablement
pub fn set_thread_highlight_sound_enabled(enabled: bool) {
    crate::backend::settings::set_thread_highlight_sound_enabled(enabled);
}

/// Set whether participated reply threads appear in Activity Mentions.
pub fn set_thread_highlight_show_in_mentions(enabled: bool) {
    crate::backend::settings::set_thread_highlight_show_in_mentions(enabled);
}

/// Get filters
pub fn get_filters() -> Vec<FilterRule> {
    crate::backend::settings::get_filters()
}

/// Add filter
pub fn add_filter(filter: FilterRule) {
    crate::backend::settings::add_filter(filter);
}

/// Update filter
pub fn update_filter(id: &str, filter: FilterRule) {
    crate::backend::settings::update_filter(id, filter);
}

/// Remove filter
pub fn remove_filter(id: &str) {
    crate::backend::settings::remove_filter(id);
}
