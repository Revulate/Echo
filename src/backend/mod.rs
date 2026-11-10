//! Echo backend: Twitch IRC and EventSub, Helix, auth, emotes, badges,
//! settings and the bridge the UI calls into.

pub(crate) mod activity_persistence;
pub mod activity_smoke;
pub mod api_error;
pub mod ascii_art;
pub mod auth;
pub mod badges;
pub mod badges_core;
pub mod blocked_users;
pub mod bridge;
pub mod browser_watching;
pub mod channel_lifecycle;
pub mod chat_replay;
pub mod chatterino_import;
pub mod chatters_core;
pub mod commands;
pub mod components;
pub mod constants;
pub mod cosmetics;
pub mod cosmetics_core;
pub mod deduplicator;
pub mod deleted_message_display;
pub mod embed_thumbnails;
pub mod embeds;
pub mod emoji;
pub mod emote_candidates;
pub mod emote_provider_visibility;
pub mod emote_quality;
pub mod emotes;
pub mod event_text;
pub mod eventsub;
pub mod fakemsg_core;
pub mod filters;
pub mod filters_replace;
pub mod followed_live;
pub mod highlight_decision;
pub mod highlight_priority;
pub mod history;
pub mod history_cache;
pub mod history_filter;
pub mod hotkeys;
pub mod http;
pub mod image_uploader;
pub mod irc;
pub mod link_types;
pub mod lock_utils;
pub mod memory_report;
pub mod metrics;
pub mod mod_buttons;
pub mod models;
pub mod moderation;
pub mod notifications;
pub mod observers;
pub mod pinned_chat;
pub mod power;
pub mod pronouns;
pub mod reliability;
pub mod retry;
pub mod scroll_markers;
pub mod search;
mod search_core;
pub mod settings;
pub mod shutdown_coordinator;
pub mod similarity;
pub mod slots;
pub mod sound_events;
pub mod sounds;
pub mod streamer_mode;
pub mod theme_files;
pub mod twitch_api;
pub mod types;
pub mod ui_actions;
pub mod updates;
pub(crate) mod upload_coordinator;
pub mod upload_history;
pub mod user_notes;
pub mod usernotice_core;
pub mod whispers;
pub mod ws;

pub use bridge::{
    Account, AuthState, BadgeDisplayMode, ChatMessage, ConnectionStatus, Emote, EmoteCategory,
    EmoteProvider, FilterAction, FilterRule, HighlightType, ModAction, ModActionType, ReplyInfo,
    SoundEventSettings, SoundEventType, StreamInfo, TabOverflowMode, TabTheme, TwitchEvent,
    UiAction, UserMessageContext, WhisperThread,
};

pub use bridge::{
    add_filter, ban_user, cancel_login, clear_all_activity, clear_chat, clear_global_mentions,
    clear_highlight_sound_muted_channels, clear_recent_automod_events, clear_recent_twitch_events,
    connect_channel, drain_pending_mentions, drain_pending_message_composer_inserts,
    drain_pending_quick_switches, drain_pending_replies, drain_pending_search_jumps,
    drain_pending_split_message_jumps, drain_pending_stream_info, drain_pending_thread_views,
    drain_pending_uploaded_links, drain_pending_whisper_composes, format_twitch_event_text,
    get_accounts, get_active_prediction_outcomes, get_all_emotes, get_auth_state, get_badge_url,
    get_categorized_emotes, get_external_badge_keys, get_filters, get_global_mentions,
    get_local_settings, get_messages, get_metrics_json, get_recent_chatters,
    get_recent_twitch_events, get_settings, get_sound_settings, get_stream_info,
    get_stream_info_async, get_tab_overflow_mode, get_unread_global_mention_count,
    get_unread_twitch_event_count, get_unread_whisper_count, get_user_message_context,
    get_whisper_threads, has_ffz_authority_badge, initialize, is_authenticated,
    is_using_ffz_authority_badge, live_uptime_minutes, load_channel_assets,
    load_channel_assets_batch, logout, manage_held_automod_message_async,
    manage_suspicious_user_status_async, mark_all_activity_read, mark_all_whisper_threads_read,
    mark_recent_twitch_events_read_for_channels, mark_visible_global_mentions_read,
    mark_whisper_thread_read, open_chatter_list_async, poll_connection_status, poll_twitch_events,
    poll_ui_actions, preload_channel_emotes_predictive, push_pending_mention,
    push_pending_message_composer_insert, push_pending_open_new_tab,
    push_pending_open_whispers_split, push_pending_quick_switch, push_pending_reply,
    push_pending_search_jump, push_pending_split_message_jump, push_pending_thread_view,
    push_pending_uploaded_link, push_pending_whisper_compose, push_ui_action,
    queue_bttv_pro_badge_lookup, queue_stream_info_refresh,
    refresh_authenticated_user_emotes_async, remove_account, remove_filter, remove_global_mention,
    remove_recent_twitch_event, reset_metrics, send_message_async, send_message_or_command_async,
    send_reply_async, set_accent_color, set_anonymous_mode, set_badge_display_mode,
    set_desktop_notifications_enabled, set_flash_on_mention, set_font_scale,
    set_global_sound_enabled, set_global_sound_volume, set_highlight_sound_channel_muted,
    set_show_mention_in_title, set_sound_event_enabled, set_sound_event_volume,
    set_tab_overflow_mode, set_tab_theme, set_theme, set_thread_highlight_enabled,
    set_thread_highlight_show_in_mentions, set_thread_highlight_sound_enabled, start_login,
    stream_info_gate_hash, switch_account, take_pending_open_new_tab,
    take_pending_open_whispers_split, test_sound, timeout_user, trim_inactive_channels,
    twitch_event_title, update_filter, upload_image_files_async, upload_pasted_image_async,
};
pub use shutdown_coordinator::{is_shutdown_started, shutdown, shutdown_once};

pub use settings::{
    CustomTheme, get_custom_theme, set_custom_theme_base_color, set_custom_theme_light_mode,
    set_custom_theme_saturation,
};
