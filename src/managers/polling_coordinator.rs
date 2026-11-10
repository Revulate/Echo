//! Polling Coordinator - Extracted from app.rs monolith
//!
//! ## Reactive Architecture (GPUI Best Practices)
//!
//! Chat messages use GPUI's reactive Entity/EventEmitter pattern:
//! - ChatBackend drains observer events into per-channel ChatState entities
//! - ChatView subscribes directly to its channel ChatState for split-local redraws
//! - App-level notifies are reserved for structural UI changes
//!
//! This coordinator still handles:
//! - Observer event processing (bridging background IRC to UI thread)
//! - Twitch events (250ms), UI actions (on wake)
//! - Connection status, auth state (1s)
//! - Stream info refresh (30s)
//! - Animation ticks (33ms adaptive via FrameRateController)
//! - Layout cleanup (30s)
//! - Image cleanup
//!
//! ## Frame Rate Control
//!
//! Uses FrameRateController for adaptive polling:
//! - Interactive: 16ms (60fps) - user actively scrolling/dragging/typing
//! - Animating: 33ms (30fps) - emotes animating but no user input
//! - FocusedIdle: 100ms (10fps) - recently active but idle
//! - DeepIdle/TrueIdle: low-frequency or event-driven idle based on real activity
//!
//! Window focus is not part of polling cadence. Losing OS focus must not delay
//! chat delivery, emote loads, 7TV live updates, layout work, or animation wakeups.
//!
//! UI actions requiring direct split/input access (reply, thread open, mention insert)
//! are forwarded to EchoApp via pending queues in backend::ui_actions.

use crate::backend;
use crate::cache::loader as image_cache;
use crate::components::chat::canvas_renderer::global_layout_cache;
use crate::config;
use crate::managers::dynamic_tick_scheduler;
use crate::managers::frame_rate_controller;
use crate::managers::{OverlayManager, refresh_auth_cache_if_changed, take_pending_popouts};
use crate::state::{AppState, EmoteStateManager, is_placeholder};
use crate::theme::colors::invalidate_theme_cache;
use echo_core::settings_event::settings_event_requires_layout_invalidation;
use gpui::*;
use rustc_hash::{FxHashMap, FxHashSet};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

static SETTINGS_EVENT_SEEN_COUNT: AtomicU64 = AtomicU64::new(0);
static LAST_SETTINGS_EVENT_LOG_MS: AtomicU64 = AtomicU64::new(0);

static SIDEBAR_REFRESH_LAST_US: AtomicU64 = AtomicU64::new(0);
static SIDEBAR_REFRESH_MAX_US: AtomicU64 = AtomicU64::new(0);
static SIDEBAR_REFRESH_LOG_TIME_MS: AtomicU64 = AtomicU64::new(0);

fn stream_info_refresh_snapshot(state: &AppState) -> (Vec<String>, Vec<(String, u64)>) {
    let mut channels: Vec<String> = state
        .all_active_channels()
        .into_iter()
        .filter(|channel| !backend::whispers::is_whispers_channel(channel))
        .collect();
    channels.sort_unstable();
    channels.dedup();
    let last_hashes = channels
        .iter()
        .filter_map(|channel| {
            state.stream_info.get(channel).map(|info| {
                (
                    channel.clone(),
                    backend::stream_info_gate_hash(info.as_ref()),
                )
            })
        })
        .collect();
    (channels, last_hashes)
}

/// Split headers print live uptime in whole minutes, but stream-info polls
/// skip unchanged results. Notify once whenever any live channel's minute
/// rolls over so the label keeps counting without a repaint loop. The
/// fingerprint covers each live (channel, minute) pair, so a channel going
/// offline in the same tick as another's rollover cannot cancel it out.
fn notify_live_uptime_rollover(
    app_state: &Entity<AppState>,
    last_uptime_fingerprint: &mut u64,
    cx: &mut AsyncApp,
) {
    use std::hash::{Hash, Hasher};

    app_state.update(cx, |state, cx| {
        let mut live_minutes: Vec<(&str, i64)> = state
            .stream_info
            .iter()
            .map(|(channel, info)| {
                (
                    channel.as_str(),
                    backend::live_uptime_minutes(info.as_ref()),
                )
            })
            .filter(|(_, minutes)| *minutes > 0)
            .collect();
        live_minutes.sort_unstable();
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        live_minutes.hash(&mut hasher);
        let fingerprint = hasher.finish();
        if fingerprint != *last_uptime_fingerprint {
            *last_uptime_fingerprint = fingerprint;
            crate::backend::metrics::record_ui_notify_source("polling.live_uptime");
            cx.notify();
        }
    });
}

fn open_external_link(url: &str, mode: backend::ui_actions::LinkOpenMode) {
    match mode {
        backend::ui_actions::LinkOpenMode::Configured => crate::utils::open_url(url),
        backend::ui_actions::LinkOpenMode::Normal => crate::utils::open_url_normal(url),
        backend::ui_actions::LinkOpenMode::Incognito => crate::utils::open_url_incognito(url),
    }
}

fn dispatch_ui_actions(
    app_state: &Entity<AppState>,
    overlay_manager: &Entity<OverlayManager>,
    cx: &mut AsyncApp,
) {
    let ui_actions = backend::poll_ui_actions();
    let needs_notify = !ui_actions.is_empty();
    for action in ui_actions {
        match action {
            backend::UiAction::Reply {
                channel,
                source_split_id,
                message_id,
                sender,
                display_name,
                content,
            } => {
                backend::push_pending_reply(crate::backend::ui_actions::PendingReply {
                    channel,
                    source_split_id,
                    message_id,
                    sender,
                    display_name,
                    content,
                });
            }
            backend::UiAction::ViewThread {
                message_id,
                channel,
                x,
                y,
            } => {
                backend::push_pending_thread_view(crate::backend::ui_actions::PendingThreadView {
                    message_id,
                    channel,
                    x,
                    y,
                });
            }
            backend::UiAction::ShowContextMenu(state) => {
                overlay_manager.update(cx, |manager, cx| {
                    manager.open_context_menu(state, cx);
                });
            }
            backend::UiAction::CloseContextMenu => {
                overlay_manager.update(cx, |manager, cx| {
                    manager.close_context_menu(cx);
                });
            }
            backend::UiAction::ShowUserCard {
                username,
                display_name,
                user_id,
                channel,
                color,
                badges,
                x,
                y,
            } => {
                use crate::components::overlays::UserCardState;
                let state = UserCardState::open(
                    username,
                    Some(display_name),
                    user_id,
                    channel,
                    color,
                    badges,
                    x,
                    y,
                );
                cx.update(|cx| {
                    crate::components::overlays::open_user_card_popout(state, cx);
                });
            }
            backend::UiAction::OpenChannelPopup { channel } => {
                let popup_app_state = app_state.clone();
                cx.update(|cx| {
                    crate::components::chat::open_channel_popup(channel, popup_app_state, cx);
                });
            }
            backend::UiAction::OpenMediaInspector { source_url } => {
                cx.update(|cx| {
                    crate::components::overlays::open_media_inspector(source_url, cx);
                });
            }
            backend::UiAction::OpenChatterListLoading { channel } => {
                overlay_manager.update(cx, |manager, cx| {
                    manager.open_chatter_list_loading(channel, cx);
                });
            }
            backend::UiAction::RefreshChatterList { channel, source } => {
                overlay_manager.update(cx, |manager, cx| {
                    manager.open_chatter_list_loading(channel.clone(), cx);
                });
                crate::managers::frame_rate_controller::record_activity();

                crate::backend::bridge::get_runtime().spawn(async move {
                    match crate::backend::commands::helix::refresh_chatter_list_source(
                        &channel, &source,
                    )
                    .await
                    {
                        crate::backend::commands::CommandResult::Success(status) => {
                            if let Some(message) = status {
                                crate::log_irc!(info, "Chatter list refreshed: {}", message);
                            }
                        }
                        crate::backend::commands::CommandResult::Error(error)
                        | crate::backend::commands::CommandResult::DeliveryUnknown(error) => {
                            crate::log_irc!(
                                warn,
                                "Failed to refresh chatter list for #{} ({}): {}",
                                channel,
                                source,
                                error
                            );
                            crate::backend::push_ui_action(
                                crate::backend::UiAction::ChatterListLoadFailed {
                                    channel: channel.clone(),
                                    source: source.clone(),
                                    error: error.clone(),
                                },
                            );
                            crate::backend::push_ui_action(
                                crate::backend::UiAction::ShowWarningToast {
                                    message: "Failed to refresh chatter list".to_string(),
                                    detail: Some(error),
                                },
                            );
                        }
                        crate::backend::commands::CommandResult::NotACommand => {}
                    }
                    crate::managers::frame_rate_controller::record_activity();
                });
            }
            backend::UiAction::ShowChatterList {
                channel,
                chatters,
                total_count,
                source,
                login_sorted,
                is_loading,
            } => {
                use crate::components::overlays::{Chatter, ChatterListSortMode, ChatterListState};
                let state = ChatterListState {
                    is_open: true,
                    channel,
                    chatters: std::sync::Arc::new(
                        chatters
                            .into_iter()
                            .map(|chatter| Chatter {
                                username: chatter.username,
                                display_name: chatter.display_name,
                                user_id: chatter.user_id.unwrap_or_default(),
                            })
                            .collect(),
                    ),
                    total_count,
                    source,
                    search_query: String::new(),
                    sort_mode: ChatterListSortMode::default(),
                    login_sorted,
                    is_loading,
                    error: None,
                };
                overlay_manager.update(cx, |manager, cx| {
                    manager.open_chatter_list(state, cx);
                });
            }
            backend::UiAction::ChatterListLoadFailed {
                channel,
                source,
                error,
            } => {
                overlay_manager.update(cx, |manager, cx| {
                    manager.chatter_list.update(cx, |list, cx| {
                        list.set_load_error(&channel, Some(source.clone()), error.clone(), cx);
                    });
                });
            }
            backend::UiAction::RemoveChatterListEntry {
                channel,
                source,
                username,
            } => {
                overlay_manager.update(cx, |manager, cx| {
                    manager.chatter_list.update(cx, |list, cx| {
                        if list.state.channel.eq_ignore_ascii_case(&channel)
                            && list.state.source.eq_ignore_ascii_case(&source)
                        {
                            let before = list.state.chatters.len();
                            std::sync::Arc::make_mut(&mut list.state.chatters).retain(|chatter| {
                                !chatter.username.eq_ignore_ascii_case(&username)
                            });
                            if list.state.chatters.len() != before {
                                list.state.total_count = list.state.total_count.saturating_sub(1);
                                list.rebuild_visible_indices();
                                cx.notify();
                            }
                        }
                    });
                });
            }
            backend::UiAction::InsertMention {
                username,
                channel,
                source_split_id,
            } => {
                backend::push_pending_mention(username, channel, source_split_id);
            }
            backend::UiAction::InsertMessageTextIntoComposer {
                channel,
                source_split_id,
                text,
                quote,
            } => {
                backend::push_pending_message_composer_insert(
                    channel,
                    source_split_id,
                    text,
                    quote,
                );
            }
            backend::UiAction::InsertUploadedLink {
                split_id,
                channel,
                url,
            } => {
                backend::push_pending_uploaded_link(split_id, channel, url);
            }
            backend::UiAction::DismissMention {
                channel,
                message_id,
            } => {
                if backend::remove_global_mention(&channel, &message_id) {
                    app_state.update(cx, |state, cx| {
                        state.refresh_sidebar_caches();
                        cx.notify();
                    });
                }
            }
            backend::UiAction::ComposeWhisper { username, channel } => {
                backend::push_pending_whisper_compose(username, channel);
            }
            backend::UiAction::OpenWhispersSplit => {
                backend::push_pending_open_whispers_split();
            }
            backend::UiAction::MarkWhisperRead { username } => {
                if backend::mark_whisper_thread_read(&username) {
                    app_state.update(cx, |state, cx| {
                        state.refresh_sidebar_caches();
                        cx.notify();
                    });
                }
            }
            backend::UiAction::JumpToMessage {
                channel,
                message_id,
            } => {
                backend::push_pending_search_jump(channel, message_id);
            }
            backend::UiAction::JumpToMessageInSplit {
                split_id,
                channel,
                message_id,
            } => {
                backend::push_pending_split_message_jump(split_id, channel, message_id);
            }
            backend::UiAction::ConfirmOpenLink { url, mode } => {
                if !backend::settings::should_confirm_open_link(&url) {
                    open_external_link(&url, mode);
                    continue;
                }

                let mode_label = match mode {
                    backend::ui_actions::LinkOpenMode::Configured => "configured browser mode",
                    backend::ui_actions::LinkOpenMode::Normal => "normal browser window",
                    backend::ui_actions::LinkOpenMode::Incognito => "private browser window",
                };
                let message =
                    format!("Echo will open this external link using {mode_label}:\n{url}");

                let host = backend::settings::external_link_host(&url);
                let url_for_confirm = url.clone();
                let on_confirm =
                    std::sync::Arc::new(move || open_external_link(&url_for_confirm, mode));
                let on_alternate = host.clone().map(|host| {
                    let url = url.clone();
                    std::sync::Arc::new(move || {
                        backend::settings::trust_link_domain(&host);
                        open_external_link(&url, mode);
                    }) as std::sync::Arc<dyn Fn() + Send + Sync + 'static>
                });
                let url_for_global_trust = url.clone();
                let on_tertiary = std::sync::Arc::new(move || {
                    backend::settings::set_skip_open_link_confirmation(true);
                    open_external_link(&url_for_global_trust, mode);
                });

                overlay_manager.update(cx, |manager, cx| {
                    manager.confirmation_modal.update(cx, |modal, cx| {
                        modal.supersede_pending();
                        modal.state = crate::components::overlays::ConfirmationState {
                            is_open: true,
                            title: "Open external link?".to_string(),
                            message,
                            confirm_text: "Open".to_string(),
                            cancel_text: "Cancel".to_string(),
                            alternate_text: host.as_ref().map(|_| "Trust domain".to_string()),
                            tertiary_text: Some("Don't ask again".to_string()),
                            is_destructive: false,
                            confirm_focused: true,
                        };
                        modal.on_confirm = Some(on_confirm);
                        modal.on_cancel = None;
                        modal.on_alternate = on_alternate;
                        modal.on_tertiary = Some(on_tertiary);
                        cx.notify();
                    });
                });
            }
            backend::UiAction::ConfirmChatterinoTabReplace { tab_count } => {
                let on_confirm: std::sync::Arc<dyn Fn() + Send + Sync> =
                    std::sync::Arc::new(backend::chatterino_import::confirm_pending_replace);
                let on_cancel: std::sync::Arc<dyn Fn() + Send + Sync> =
                    std::sync::Arc::new(backend::chatterino_import::cancel_pending_replace);
                let tabs = if tab_count == 1 { "tab" } else { "tabs" };
                overlay_manager.update(cx, |manager, cx| {
                    manager.confirmation_modal.update(cx, |modal, cx| {
                        modal.open_simple(
                            "Replace your tabs?".to_string(),
                            format!(
                                "Echo will close your open tabs and show the {tab_count} imported {tabs}. Your tabs are backed up first, and Reopen Closed Tab brings them back until Echo closes."
                            ),
                            "Replace tabs".to_string(),
                            true,
                            on_confirm,
                            cx,
                        );
                        modal.on_cancel = Some(on_cancel);
                    });
                });
            }
            backend::UiAction::ConfirmCloseTabs {
                tab_id,
                to_right,
                count,
            } => {
                let on_confirm = std::sync::Arc::new(move || {
                    backend::ui_actions::push_pending_bulk_tab_close(tab_id.clone(), to_right);
                });
                let tabs = if count == 1 { "tab" } else { "tabs" };
                let (title, message) = if to_right {
                    (
                        format!("Close {count} {tabs} to the right?"),
                        format!(
                            "Echo will close {count} {tabs} to the right of this one. Reopen Closed Tab brings them back."
                        ),
                    )
                } else {
                    (
                        format!("Close {count} other {tabs}?"),
                        "Echo will close every tab except this one. Reopen Closed Tab brings them back.".to_string(),
                    )
                };
                overlay_manager.update(cx, |manager, cx| {
                    manager.confirmation_modal.update(cx, |modal, cx| {
                        modal.open_simple(
                            title,
                            message,
                            "Close tabs".to_string(),
                            true,
                            on_confirm,
                            cx,
                        );
                    });
                });
            }
            backend::UiAction::ConfirmClearUploadHistory => {
                let on_confirm = std::sync::Arc::new(|| {
                    let removed = backend::upload_history::clear_history();
                    backend::push_ui_action(backend::UiAction::ShowSuccessToast {
                        message: "Upload history cleared".to_string(),
                        detail: Some(format!("Forgot {removed} local upload records")),
                    });
                });
                overlay_manager.update(cx, |manager, cx| {
                    manager.confirmation_modal.update(cx, |modal, cx| {
                        modal.open_simple("Clear upload history?".to_string(), "This forgets Echo's local upload records. It does not delete files from their hosting providers.".to_string(), "Clear history".to_string(), true, on_confirm, cx);
                    });
                });
            }
            backend::UiAction::ConfirmClearReadableLogs => {
                let on_confirm = std::sync::Arc::new(|| {
                    spawn_clear_files(
                        backend::history_cache::clear_readable_logs,
                        "Readable channel logs cleared",
                        "log",
                        "Failed to clear readable channel logs",
                    );
                });
                overlay_manager.update(cx, |manager, cx| {
                    manager.confirmation_modal.update(cx, |modal, cx| {
                        modal.open_simple("Clear readable channel logs?".to_string(), "Echo will delete the daily chat log files it wrote in your log folder. Other files in that folder are left alone.".to_string(), "Delete logs".to_string(), true, on_confirm, cx);
                    });
                });
            }
            backend::UiAction::ConfirmClearHistoryExports => {
                let on_confirm = std::sync::Arc::new(|| {
                    spawn_clear_files(
                        backend::history_cache::clear_history_text_exports,
                        "History text exports cleared",
                        "export",
                        "Failed to clear history text exports",
                    );
                });
                overlay_manager.update(cx, |manager, cx| {
                    manager.confirmation_modal.update(cx, |modal, cx| {
                        modal.open_simple("Clear history text exports?".to_string(), "Echo will delete the history text exports it wrote to its exports folder.".to_string(), "Delete exports".to_string(), true, on_confirm, cx);
                    });
                });
            }
            backend::UiAction::ConfirmResetHotkeys => {
                let on_confirm = std::sync::Arc::new(|| {
                    backend::hotkeys::reset_all_bindings();
                });
                overlay_manager.update(cx, |manager, cx| {
                    manager.confirmation_modal.update(cx, |modal, cx| {
                        modal.open_simple("Reset all hotkeys?".to_string(), "Every built-in hotkey goes back to its default binding. Custom command hotkeys are kept.".to_string(), "Reset hotkeys".to_string(), true, on_confirm, cx);
                    });
                });
            }
            backend::UiAction::ConfirmUploaderMappingReplacement {
                saved,
                replacement,
                draft_generation,
                origin,
                origin_window,
                status_revision,
            } => {
                let cancel_origin = origin.clone();
                let cancel_origin_host = origin.entity_id().as_u64();
                let on_confirm = std::sync::Arc::new(move || {
                    let applied = if origin.is_upgradable() {
                        crate::components::settings::confirm_uploader_mapping(
                            &saved.0,
                            &replacement.0,
                            draft_generation,
                            origin.entity_id().as_u64(),
                            status_revision,
                        )
                    } else {
                        crate::log_ui!(
                            debug,
                            "Ignored uploader confirmation for a closed settings host"
                        );
                        false
                    };
                    backend::push_ui_action(
                        backend::UiAction::UploaderMappingConfirmationResolved {
                            origin: origin.clone(),
                            origin_window,
                            applied,
                            status_revision,
                        },
                    );
                });
                let on_cancel = std::sync::Arc::new(move || {
                    backend::push_ui_action(
                        backend::UiAction::UploaderMappingConfirmationResolved {
                            origin: cancel_origin.clone(),
                            origin_window,
                            applied: false,
                            status_revision,
                        },
                    );
                });
                overlay_manager.update(cx, |manager, cx| {
                    manager.confirmation_modal.update(cx, |modal, cx| {
                        modal.open_simple("Replace your saved uploader mapping?".to_string(), "Your saved request and response mapping will be replaced. Importing a preset without an Echo extension may also enable uploads, paste uploads, and drag-and-drop uploads. Cancel keeps the current mapping.".to_string(), "Replace mapping".to_string(), true, on_confirm, cx);
                        modal.uploader_owner = Some(cancel_origin_host);
                        modal.on_cancel = Some(on_cancel);
                    });
                });
            }
            backend::UiAction::CancelUploaderConfirmation { host } => {
                overlay_manager.update(cx, |manager, cx| {
                    manager.confirmation_modal.update(cx, |modal, cx| {
                        if modal.state.is_open && modal.uploader_owner == Some(host) {
                            modal.supersede_pending();
                            cx.notify();
                        }
                    });
                });
            }
            backend::UiAction::UploaderMappingConfirmationResolved {
                origin,
                origin_window,
                applied,
                status_revision,
            } => {
                crate::components::settings::resolve_uploader_status(
                    origin.entity_id().as_u64(),
                    status_revision,
                    applied,
                );
                let config = backend::settings::get_image_uploader_config();
                let target =
                    crate::components::settings::uploader_confirmation_target(applied, &config);
                cx.update(|cx| {
                    crate::components::settings::notify_uploader_confirmation(
                        origin,
                        origin_window,
                        target,
                        cx,
                    )
                });
            }
            backend::UiAction::ConfirmResetModButtons => {
                let on_confirm = std::sync::Arc::new(|| {
                    crate::components::settings::reset_mod_buttons_to_defaults();
                });
                overlay_manager.update(cx, |manager, cx| {
                    manager.confirmation_modal.update(cx, |modal, cx| {
                        modal.open_simple(
                            "Reset moderation buttons?".to_string(),
                            "Your moderation buttons will be replaced with the default set."
                                .to_string(),
                            "Reset buttons".to_string(),
                            true,
                            on_confirm,
                            cx,
                        );
                    });
                });
            }
            backend::UiAction::ConfirmClearMutedSounds => {
                let on_confirm = std::sync::Arc::new(|| {
                    backend::clear_highlight_sound_muted_channels();
                });
                overlay_manager.update(cx, |manager, cx| {
                    manager.confirmation_modal.update(cx, |modal, cx| {
                        modal.open_simple(
                            "Unmute all channels?".to_string(),
                            "Sounds and desktop notifications will be allowed again in every channel you muted."
                                .to_string(),
                            "Unmute all".to_string(),
                            true,
                            on_confirm,
                            cx,
                        );
                    });
                });
            }
            backend::UiAction::ConfirmClearMessageCache => {
                let on_confirm = std::sync::Arc::new(|| {
                    crate::components::settings::clear_message_cache();
                });
                overlay_manager.update(cx, |manager, cx| {
                    manager.confirmation_modal.update(cx, |modal, cx| {
                        modal.open_simple(
                            "Clear message cache?".to_string(),
                            "Echo will delete the chat history it saved on disk for every channel."
                                .to_string(),
                            "Clear cache".to_string(),
                            true,
                            on_confirm,
                            cx,
                        );
                    });
                });
            }
            backend::UiAction::ConfirmClearModLog => {
                let on_confirm = std::sync::Arc::new(|| {
                    backend::bridge::clear_mod_actions();
                });
                overlay_manager.update(cx, |manager, cx| {
                    manager.confirmation_modal.update(cx, |modal, cx| {
                        modal.open_simple("Clear moderation log?".to_string(), "All moderation actions recorded this session will be removed from the log.".to_string(), "Clear log".to_string(), true, on_confirm, cx);
                    });
                });
            }
            backend::UiAction::OpenNotifications { filter } => {
                app_state.update(cx, |state, cx| {
                    state.open_activity(crate::state::ActivitySection::Events, filter, cx);
                });
            }
            backend::UiAction::ConfirmRemoveAccount {
                username,
                display_name,
            } => {
                let username_for_remove = username.clone();
                let on_confirm = std::sync::Arc::new(move || {
                    backend::remove_account(&username_for_remove);
                });
                let title = "Remove Twitch account?".to_string();
                let message = if backend::streamer_mode::should_hide_account_names() {
                    "Echo will remove this saved Twitch account from the local account list. You can add it again by logging in with Twitch."
                            .to_string()
                } else {
                    format!(
                        "Echo will remove {display_name} (@{username}) from the local account list. You can add it again by logging in with Twitch."
                    )
                };

                overlay_manager.update(cx, |manager, cx| {
                    manager.confirmation_modal.update(cx, |modal, cx| {
                        modal.open_simple(
                            title,
                            message,
                            "Remove".to_string(),
                            true,
                            on_confirm,
                            cx,
                        );
                    });
                });
            }
            backend::UiAction::ConfirmBlockUser {
                user_id,
                username,
                display_name,
                expected_owner_user_id,
            } => {
                let user = backend::blocked_users::BlockedUser {
                    user_id: user_id.trim().to_string(),
                    user_login: username.trim().trim_start_matches('@').to_ascii_lowercase(),
                    display_name: display_name.trim().to_string(),
                };
                let session = match backend::blocked_users::capture_active_session() {
                    Ok(session) => session,
                    Err(error) => {
                        backend::push_ui_action(backend::UiAction::ShowWarningToast {
                            message: "Block unavailable".to_string(),
                            detail: Some(error),
                        });
                        continue;
                    }
                };
                if user.user_id.is_empty()
                    || user.user_login.is_empty()
                    || user.user_id == session.owner_user_id()
                    || expected_owner_user_id != session.owner_user_id()
                {
                    backend::push_ui_action(backend::UiAction::ShowWarningToast {
                        message: "Block unavailable".to_string(),
                        detail: Some("A different stable Twitch user is required.".to_string()),
                    });
                    continue;
                }
                if backend::blocked_users::is_blocked(Some(&user.user_id), &user.user_login) {
                    backend::push_ui_action(backend::UiAction::ShowSuccessToast {
                        message: "User is already blocked on Twitch".to_string(),
                        detail: None,
                    });
                    continue;
                }

                let on_confirm = std::sync::Arc::new(move || {
                    backend::blocked_users::set_block_state_async_for_session(
                        session.clone(),
                        user.clone(),
                        true,
                    );
                });

                overlay_manager.update(cx, |manager, cx| {
                    manager.confirmation_modal.update(cx, |modal, cx| {
                        modal.open_simple("Block Twitch user?".to_string(), "Echo will block this Twitch user on the active account. Their chat messages and whispers will be hidden according to your blocked-user settings.".to_string(), "Block".to_string(), true, on_confirm, cx);
                    });
                });
            }
            backend::UiAction::ConfirmResetSettings => {
                let on_confirm = std::sync::Arc::new(|| {
                    match backend::settings::reset_settings_to_defaults() {
                        Ok(result) => {
                            crate::components::settings::reset_migrated_editor_drafts_after_settings_replace();
                            crate::log_ui!(
                                info,
                                backup_path = ?result.backup_path,
                                "Reset settings to defaults"
                            );
                            let detail = result
                                .backup_path
                                .map(|path| format!("Backup saved to {}", path.display()));
                            backend::push_ui_action(backend::UiAction::ShowSuccessToast {
                                message: "Settings reset to defaults".to_string(),
                                detail,
                            });
                        }
                        Err(error) => {
                            backend::push_ui_action(backend::UiAction::ShowWarningToast {
                                message: "Failed to reset settings".to_string(),
                                detail: Some(error),
                            });
                        }
                    }
                });

                overlay_manager.update(cx, |manager, cx| {
                    manager.confirmation_modal.update(cx, |modal, cx| {
                        modal.open_simple("Reset all settings?".to_string(), "Echo will restore default settings and keep your current Twitch accounts. A backup is created before the reset.".to_string(), "Reset".to_string(), true, on_confirm, cx);
                    });
                });
            }
            backend::UiAction::OpenUpdatesSettings => {
                overlay_manager.update(cx, |manager, cx| {
                    manager.open_settings(
                        Some(crate::components::settings::SettingsSectionId::Updates),
                        cx,
                    );
                });
            }
            backend::UiAction::UpdateStateChanged => {}
            backend::UiAction::WatchingChannelChanged { channel } => {
                if let Some(channel) = channel.as_deref() {
                    let is_consumed =
                        cx.update(|cx| app_state.read(cx).all_active_channels().contains(channel));
                    if is_consumed {
                        backend::preload_channel_emotes_predictive(channel);
                        backend::connect_channel(channel);
                    }
                    crate::log_ui!(
                        info,
                        channel,
                        source = crate::backend::browser_watching::current_source()
                            .map(|source| source.label())
                            .unwrap_or("none"),
                        is_consumed,
                        "Watching split followed the active Twitch browser channel"
                    );
                } else {
                    crate::log_ui!(
                        info,
                        "Watching split is waiting for an active Twitch browser channel"
                    );
                }
            }
            backend::UiAction::ShowUpdateAvailableToast { version } => {
                use crate::components::overlays::Toast;

                let toast = Toast::info(format!("Echo {version} is available"))
                    .with_detail("Click to view the signed update and changelog")
                    .with_duration(10)
                    .with_on_click(|| {
                        backend::push_ui_action(backend::UiAction::OpenUpdatesSettings);
                    });

                overlay_manager.update(cx, |manager, cx| {
                    manager.push_toast(toast, cx);
                });
            }
            backend::UiAction::ShowSuccessToast { message, detail } => {
                use crate::components::overlays::Toast;

                let toast = Toast::success(message);
                let toast = match detail {
                    Some(detail) => toast.with_detail(detail),
                    None => toast,
                };

                overlay_manager.update(cx, |manager, cx| {
                    manager.push_toast(toast, cx);
                });
            }
            backend::UiAction::ShowWarningToast { message, detail } => {
                use crate::components::overlays::Toast;

                let toast = Toast::warning(message);
                let toast = match detail {
                    Some(detail) => toast.with_detail(detail),
                    None => toast,
                };

                overlay_manager.update(cx, |manager, cx| {
                    manager.push_toast(toast, cx);
                });
            }
            backend::UiAction::ShowEmoteDisplayRuleToast { message, undo } => {
                use crate::components::overlays::Toast;

                let toast = Toast::info(message).with_action("Undo", move || {
                    backend::settings::apply_emote_display_rule_changes(&undo);
                });

                overlay_manager.update(cx, |manager, cx| {
                    manager.push_toast(toast, cx);
                });
            }
            backend::UiAction::ExplainHighlight { highlight_type } => {
                use crate::components::overlays::Toast;
                use echo_core::highlight_explanation::{explain_highlight, highlight_rule_origin};

                let explanation = explain_highlight(&highlight_type);
                let mut toast = Toast::info(explanation.title).with_duration(10);
                if let Some(detail) = explanation.detail {
                    toast = toast.with_detail(detail);
                }
                if highlight_rule_origin(&highlight_type).is_some() {
                    toast = toast.with_action("Edit rule", move || {
                        backend::push_ui_action(backend::UiAction::EditHighlightRule {
                            highlight_type: highlight_type.clone(),
                        });
                    });
                }

                overlay_manager.update(cx, |manager, cx| {
                    manager.push_toast(toast, cx);
                });
            }
            backend::UiAction::EditHighlightRule { highlight_type } => {
                use crate::components::overlays::Toast;

                let rule = echo_core::highlight_explanation::highlight_rule_origin(&highlight_type)
                    .and_then(backend::settings::find_highlight_rule_for_origin);
                let open = crate::components::settings::highlight_rule_editor_request(rule);

                overlay_manager.update(cx, |manager, cx| {
                    manager.open_settings_at(open.request, cx);
                    if let Some((title, detail)) = open.problem {
                        manager.push_toast(
                            Toast::warning(title).with_detail(detail).with_duration(8),
                            cx,
                        );
                    }
                });
            }
        }
    }
    if needs_notify {
        app_state.update(cx, |_state, cx| {
            crate::backend::metrics::record_ui_notify_source("polling.ui_actions");
            cx.notify();
        });
    }
}

/// Set by a finished background clear so the next poll refreshes every
/// window, including a settings popout the main window's notify never reaches.
static CLEAR_FILES_REFRESH_PENDING: AtomicBool = AtomicBool::new(false);

/// Run a clear action off the UI thread, then report it and refresh the
/// logging section.
fn spawn_clear_files(
    clear: fn() -> Result<backend::history_cache::ClearedFiles, String>,
    success: &'static str,
    noun: &'static str,
    failure: &'static str,
) {
    let spawned = std::thread::Builder::new()
        .name("echo-clear-files".to_string())
        .spawn(move || {
            let result = clear();
            crate::components::settings::invalidate_logging_scan();
            CLEAR_FILES_REFRESH_PENDING.store(true, Ordering::Release);
            push_clear_files_result(result, success, noun, failure);
        });
    if let Err(error) = spawned {
        push_clear_files_result(
            Err(format!("Could not start the clear task: {error}")),
            success,
            noun,
            failure,
        );
    }
}

fn push_clear_files_result(
    result: Result<backend::history_cache::ClearedFiles, String>,
    success: &str,
    noun: &str,
    failure: &str,
) {
    let action = match result {
        Ok(cleared) if cleared.failed == 0 => backend::UiAction::ShowSuccessToast {
            message: success.to_string(),
            detail: Some(format!("Removed {} {noun} file(s)", cleared.removed)),
        },
        Ok(cleared) => {
            crate::log_persist!(
                warn,
                "{failure}: removed {}, failed {}",
                cleared.removed,
                cleared.failed
            );
            backend::UiAction::ShowWarningToast {
                message: failure.to_string(),
                detail: Some(format!(
                    "Removed {}, failed {} {noun} file(s)",
                    cleared.removed, cleared.failed
                )),
            }
        }
        Err(error) => {
            crate::log_persist!(warn, "{failure}: {error}");
            backend::UiAction::ShowWarningToast {
                message: failure.to_string(),
                detail: Some(error),
            }
        }
    };
    backend::push_ui_action(action);
}

#[inline]
fn settings_event_requires_picker_refresh(event: &crate::backend::settings::SettingsEvent) -> bool {
    matches!(
        event,
        crate::backend::settings::SettingsEvent::AnimationPerformanceChanged
    )
}

#[inline]
fn settings_event_requires_reduce_motion_sync(
    event: &crate::backend::settings::SettingsEvent,
) -> bool {
    matches!(
        event,
        crate::backend::settings::SettingsEvent::AnimationPerformanceChanged
    )
}

fn handle_settings_event_side_effects(event: &crate::backend::settings::SettingsEvent) {
    use crate::backend::settings::SettingsEvent;
    match event {
        SettingsEvent::ThemeChanged => invalidate_theme_cache(),
        SettingsEvent::ProxyChanged => {
            backend::eventsub::reconnect_for_proxy_change();
            backend::emotes::seventv_eventapi::restart_for_proxy_change();
            backend::irc::reconnect_with_current_auth();
        }
        _ => {}
    }
}

#[derive(Default)]
struct DrainedSettingsEvents {
    saw_event: bool,
    needs_invalidation: bool,
    needs_picker_refresh: bool,
    needs_reduce_motion_sync: bool,
}

/// Drain pending settings changes while keeping each polling mode responsible
/// for its own notification and activity policy.
fn drain_settings_events(
    rx: &mut tokio::sync::broadcast::Receiver<crate::backend::settings::SettingsEvent>,
    mut before_side_effects: impl FnMut(&crate::backend::settings::SettingsEvent),
) -> DrainedSettingsEvents {
    let mut drained = DrainedSettingsEvents::default();

    while let Ok(event) = rx.try_recv() {
        before_side_effects(&event);
        handle_settings_event_side_effects(&event);
        drained.needs_invalidation |= settings_event_requires_layout_invalidation(&event);
        drained.needs_picker_refresh |= settings_event_requires_picker_refresh(&event);
        drained.needs_reduce_motion_sync |= settings_event_requires_reduce_motion_sync(&event);
        drained.saw_event = true;
    }

    drained
}

#[inline]
fn maybe_reload_custom_theme_file(last_reload_check: &mut Instant, interval: Duration) {
    if last_reload_check.elapsed() < interval {
        return;
    }
    *last_reload_check = Instant::now();
    if let Err(err) = backend::settings::reload_custom_theme_file_if_changed() {
        crate::log_ui!(warn, "Theme file auto-reload failed: {}", err);
    }
}

#[inline]
fn maybe_refresh_system_theme(last_check: &mut Instant, interval: Duration) {
    if last_check.elapsed() < interval || !backend::settings::is_system_theme_active() {
        return;
    }
    *last_check = Instant::now();
    let _ = backend::settings::refresh_system_theme_resolution(false);
}

#[inline]
/// A lane that has never run (`None`) is due immediately.
fn poll_due(last: Option<Instant>, interval: Duration) -> bool {
    last.is_none_or(|last| last.elapsed() >= interval)
}

fn maybe_refresh_streamer_detection(last_check: &mut Option<Instant>, interval: Duration) {
    if !poll_due(*last_check, interval)
        || !matches!(
            backend::streamer_mode::get_streamer_mode_settings().enabled,
            backend::streamer_mode::StreamerModeStatus::Auto
        )
    {
        return;
    }
    *last_check = Some(Instant::now());
    let _ = backend::streamer_mode::is_streaming_software_running();
}

/// Change key for the Activity sidebar snapshots owned by AppState.
///
/// `refresh_sidebar_caches` rebuilds both Vecs unconditionally, so buffer
/// identity cannot distinguish a real change from a rebuild of identical rows.
fn sidebar_cache_signature(state: &AppState) -> u64 {
    let mut hasher = DefaultHasher::new();
    state.cached_mentions.len().hash(&mut hasher);
    for (channel, message) in state.cached_mentions.iter() {
        channel.hash(&mut hasher);
        message.id.hash(&mut hasher);
        (Arc::as_ptr(&message.slots).cast::<()>() as usize).hash(&mut hasher);
    }
    state.cached_whispers.len().hash(&mut hasher);
    for thread in state.cached_whispers.iter() {
        thread.username.hash(&mut hasher);
        thread.unread_count.hash(&mut hasher);
        thread.last_timestamp.hash(&mut hasher);
        thread.last_message.hash(&mut hasher);
    }
    hasher.finish()
}

/// Coordinator for background polling operations
///
/// This is the "central nervous system" of the application, responsible for:
/// - Checking backend state at various intervals
/// - Updating app state when changes are detected
/// - Batching notifications to minimize re-renders
/// - Coordinating with managers via Entity updates
/// - Bridging emote loading events to the GPUI reactive system
pub struct PollingCoordinator {
    app_state: Entity<AppState>,
    overlay_manager: Entity<OverlayManager>,
    emote_state: Option<Entity<EmoteStateManager>>,
    _task: Option<Task<()>>,
}

impl PollingCoordinator {
    pub fn new(app_state: Entity<AppState>, overlay_manager: Entity<OverlayManager>) -> Self {
        Self {
            app_state,
            overlay_manager,
            emote_state: None,
            _task: None,
        }
    }

    /// Set the EmoteStateManager entity (called from EchoApp after creation)
    pub fn set_emote_state(&mut self, emote_state: Entity<EmoteStateManager>) {
        self.emote_state = Some(emote_state);
    }

    /// Start the polling loop as a background task
    pub fn start(&mut self, cx: &mut Context<Self>) {
        if !polling_allowed(crate::benchmark_profile::network_lanes_suppressed()) {
            crate::log_ui!(
                info,
                "Offline benchmark suppressed PollingCoordinator network lanes"
            );
            return;
        }
        let app_state = self.app_state.clone();
        let overlay_manager = self.overlay_manager.clone();
        let emote_state = self.emote_state.clone();

        let task = cx.spawn(async move |this, cx| {
            let mut last_event_poll = Instant::now();
            let mut last_status_poll = Instant::now();
            let mut last_emote_poll = Instant::now();
            let mut last_stream_info: Option<Instant> = None;
            let mut last_uptime_fingerprint: u64 = 0;
            let mut last_image_cleanup = Instant::now();
            let mut last_layout_cleanup = Instant::now();
            let mut last_sidebar_refresh = Instant::now();
            let mut last_empty_channel_recovery: Option<Instant> = None;
            let mut last_theme_file_reload_check = Instant::now();
            let mut last_system_theme_check = Instant::now();
            let mut last_streamer_detection_check: Option<Instant> = None;

            let mut tracked_global_emote_version: u32 = 0;
            let mut tracked_user_emote_version: u64 = 0;
            let mut tracked_channel_emote_versions: FxHashMap<String, u32> = FxHashMap::default();

            let mut initialized_channels: FxHashSet<String> = FxHashSet::default();
            let mut tracked_channels_key: Option<u64> = None;

            let event_interval = config::event_poll_interval();
            let status_interval = config::status_poll_interval();
            let stream_info_interval = config::stream_info_interval();
            let image_cleanup_interval = Duration::from_secs(60);
            let layout_cleanup_interval = Duration::from_secs(30);
            let sidebar_refresh_interval = Duration::from_secs(2);
            let emote_poll_interval = Duration::from_millis(250);
            let empty_channel_recovery_interval = Duration::from_millis(500);
            let theme_file_reload_interval = Duration::from_secs(2);
            let system_theme_poll_interval = Duration::from_secs(2);
            let streamer_detection_interval = Duration::from_secs(2);

            let mut settings_rx = backend::settings::subscribe_settings();

            loop {
                let render_mode = frame_rate_controller::current_mode_cached();

                if render_mode.is_event_driven() {
                    let wakeup = frame_rate_controller::get_wakeup_notify();
                    let mut health_interval =
                        if backend::settings::get_custom_theme_file_auto_reload()
                            || backend::settings::is_system_theme_active()
                        {
                            theme_file_reload_interval
                        } else {
                            Duration::from_secs(30)
                        };
                    if matches!(
                        backend::streamer_mode::get_streamer_mode_settings().enabled,
                        backend::streamer_mode::StreamerModeStatus::Auto
                    ) {
                        health_interval = health_interval.min(streamer_detection_interval);
                    }
                    health_interval = health_interval.min(stream_info_interval);

                    let woken_by_activity = tokio::select! {
                        _ = wakeup.notified() => {
                            crate::log_perf!(debug, "Woken from {:?} mode", render_mode);
                            true
                        }
                        _ = cx.background_executor().timer(health_interval) => {
                            false
                        }
                    };

                    dispatch_ui_actions(&app_state, &overlay_manager, cx);

                    if last_status_poll.elapsed() >= status_interval {
                        last_status_poll = Instant::now();
                        let _ = backend::poll_connection_status();
                    }
                    maybe_reload_custom_theme_file(
                        &mut last_theme_file_reload_check,
                        theme_file_reload_interval,
                    );
                    maybe_refresh_system_theme(
                        &mut last_system_theme_check,
                        system_theme_poll_interval,
                    );
                    maybe_refresh_streamer_detection(
                        &mut last_streamer_detection_check,
                        streamer_detection_interval,
                    );

                    let should_recover_empty_channels =
                        poll_due(last_empty_channel_recovery, empty_channel_recovery_interval);

                    if woken_by_activity || should_recover_empty_channels {
                        if let Err(e) = this.update(cx, |coordinator, cx| {
                            let channels = coordinator.app_state.read(cx).active_tab().channels();

                            let new_channels: Vec<String> = channels
                                .iter()
                                .filter(|ch| !initialized_channels.contains(*ch))
                                .cloned()
                                .collect();

                            if !new_channels.is_empty() {
                                crate::log_irc!(
                                    debug,
                                    "Initializing {} new channels in {:?} mode",
                                    new_channels.len(),
                                    render_mode
                                );
                                coordinator.app_state.update(cx, |state, cx| {
                                    state.chat_backend.update(cx, |backend_entity, cx| {
                                        for channel in &new_channels {
                                            backend_entity.get_or_create_channel(channel, cx);
                                            initialized_channels.insert(channel.clone());
                                        }
                                    });
                                });
                            }

                            let has_pending = coordinator
                                .app_state
                                .read(cx)
                                .chat_backend
                                .read(cx)
                                .has_pending_events();

                            if has_pending {
                                let processed = coordinator.app_state.update(cx, |state, cx| {
                                    state.chat_backend.update(cx, |backend, cx| {
                                        backend.process_observer_events(cx)
                                    })
                                });
                                if processed {
                                    crate::log_irc!(
                                        debug,
                                        "Processed observer events in {:?} mode",
                                        render_mode
                                    );
                                    frame_rate_controller::record_activity();
                                }
                            }

                            if should_recover_empty_channels {
                                let recovered = coordinator.app_state.update(cx, |state, cx| {
                                    state.chat_backend.update(cx, |backend, cx| {
                                        backend.recover_empty_channels_from_backend(&channels, cx)
                                    })
                                });
                                if recovered {
                                    frame_rate_controller::record_activity();
                                    crate::backend::metrics::record_ui_notify_source(
                                        "polling.idle.recover_empty_channels",
                                    );
                                    coordinator.app_state.update(cx, |_state, cx| {
                                        cx.notify();
                                    });
                                }
                            }

                            if crate::components::overlays::drain_image_load_notifications() {
                                crate::backend::metrics::record_ui_notify_source(
                                    "polling.idle.image_ready.drained",
                                );
                                let emote_picker =
                                    coordinator.overlay_manager.read(cx).emote_picker.clone();
                                if emote_picker.read(cx).state.is_open {
                                    crate::backend::metrics::record_ui_notify_source(
                                        "polling.idle.image_ready.emote_picker",
                                    );
                                    emote_picker.update(cx, |_, cx| cx.notify());
                                }
                                if crate::components::overlays::refresh_open_emote_picker_popout(cx)
                                {
                                    crate::backend::metrics::record_ui_notify_source(
                                        "polling.idle.image_ready.emote_picker_popout",
                                    );
                                }
                            }
                        }) {
                            crate::log_ui!(
                                error,
                                "Failed to process observer events in idle mode: {:?}",
                                e
                            );
                        }
                        if should_recover_empty_channels {
                            last_empty_channel_recovery = Some(Instant::now());
                        }

                        if let Some(ref mut rx) = settings_rx {
                            let settings_events = drain_settings_events(rx, |_| {});
                            if settings_events.needs_reduce_motion_sync {
                                cx.update(|cx| {
                                    cx.set_reduce_motion(
                                        backend::settings::get_reduce_decorative_motion(),
                                    );
                                });
                            }
                            if settings_events.needs_invalidation {
                                let _ = global_layout_cache::maybe_invalidate_for_settings_change();
                            }
                            if settings_events.saw_event {
                                app_state.update(cx, |_state, cx| {
                                    crate::backend::metrics::record_ui_notify_source(
                                        "polling.idle.settings",
                                    );
                                    cx.notify();
                                });
                            }
                            if settings_events.needs_picker_refresh {
                                let _ = this.update(cx, |_coordinator, cx| {
                                    if crate::components::overlays::refresh_open_emote_picker_popout(
                                        cx,
                                    ) {
                                        crate::backend::metrics::record_ui_notify_source(
                                            "polling.idle.settings.emote_picker_popout",
                                        );
                                    }
                                });
                            }
                        }
                    }

                    if poll_due(last_stream_info, stream_info_interval) {
                        let (channels, last_hashes) = app_state
                            .update(cx, |state, _| stream_info_refresh_snapshot(state));
                        if backend::queue_stream_info_refresh(channels, last_hashes) {
                            last_stream_info = Some(Instant::now());
                        }
                    }
                    if let Some(updates) = backend::drain_pending_stream_info() {
                        app_state.update(cx, |state, state_cx| {
                            let applied = state.apply_stream_info_updates(updates);
                            if applied > 0 {
                                crate::log_ui!(
                                    debug,
                                    "stream info applied: {} channels",
                                    applied
                                );
                                crate::backend::metrics::record_ui_notify_source(
                                    "polling.stream_info",
                                );
                                state_cx.notify();
                            }
                        });
                    }
                    notify_live_uptime_rollover(&app_state, &mut last_uptime_fingerprint, cx);

                    continue;
                }

                let timer_interval = render_mode.poll_interval();
                let timer_sleep = if render_mode == frame_rate_controller::RenderMode::Animating {
                    dynamic_tick_scheduler::aligned_tick_delay(timer_interval.as_millis() as u64)
                } else {
                    timer_interval
                };
                let wakeup = frame_rate_controller::get_wakeup_notify();
                let woke_by_activity = tokio::select! {
                    _ = cx.background_executor().timer(timer_sleep) => false,
                    _ = wakeup.notified() => true,
                };

                if woke_by_activity {
                    crate::log_perf!(trace, "Woken from {:?} polling sleep", render_mode);
                }

                dispatch_ui_actions(&app_state, &overlay_manager, cx);

                if frame_rate_controller::should_skip_frame() {
                    continue;
                }

                let is_deep_idle = render_mode == frame_rate_controller::RenderMode::DeepIdle;
                if is_deep_idle {
                    if last_status_poll.elapsed() >= status_interval {
                        last_status_poll = Instant::now();
                        let _ = backend::poll_connection_status();
                    }
                    maybe_reload_custom_theme_file(
                        &mut last_theme_file_reload_check,
                        theme_file_reload_interval,
                    );
                    if let Ok(has_events) = this.update(cx, |coordinator, cx| {
                        coordinator
                            .app_state
                            .read(cx)
                            .chat_backend
                            .read(cx)
                            .has_pending_events()
                    })
                        && has_events {
                            frame_rate_controller::record_activity();
                    }
                    if let Some(ref mut rx) = settings_rx {
                        let settings_events = drain_settings_events(rx, |_| {});
                        if settings_events.needs_reduce_motion_sync {
                            cx.update(|cx| {
                                cx.set_reduce_motion(
                                    backend::settings::get_reduce_decorative_motion(),
                                );
                            });
                        }
                        if settings_events.needs_invalidation {
                            let _ = global_layout_cache::maybe_invalidate_for_settings_change();
                        }
                        if settings_events.saw_event {
                            app_state.update(cx, |_state, cx| {
                                crate::backend::metrics::record_ui_notify_source(
                                    "polling.deep_idle.settings",
                                );
                                cx.notify();
                            });
                            frame_rate_controller::record_activity();
                        }
                        if settings_events.needs_picker_refresh {
                            let _ = this.update(cx, |_coordinator, cx| {
                                if crate::components::overlays::refresh_open_emote_picker_popout(cx)
                                {
                                    crate::backend::metrics::record_ui_notify_source(
                                        "polling.deep_idle.settings.emote_picker_popout",
                                    );
                                }
                            });
                        }
                    }
                }

                let mut needs_notify = false;

                maybe_refresh_streamer_detection(
                    &mut last_streamer_detection_check,
                    streamer_detection_interval,
                );

                let has_animations = image_cache::has_visible_animations();
                if !has_animations {
                    dynamic_tick_scheduler::reset_to_default();
                }

                if crate::components::overlays::drain_image_load_notifications() {
                    crate::backend::metrics::record_ui_notify_source("polling.image_ready.drained");
                    overlay_manager.update(cx, |manager, cx| {
                        let emote_picker = manager.emote_picker.clone();
                        if emote_picker.read(cx).state.is_open {
                            crate::backend::metrics::record_ui_notify_source(
                                "polling.image_ready.emote_picker",
                            );
                            emote_picker.update(cx, |_, cx| cx.notify());
                        }
                        if crate::components::overlays::refresh_open_emote_picker_popout(cx) {
                            crate::backend::metrics::record_ui_notify_source(
                                "polling.image_ready.emote_picker_popout",
                            );
                        }
                    });
                }

                if let Some(ref mut rx) = settings_rx {
                    let settings_events = drain_settings_events(rx, |event| {
                        let seen = SETTINGS_EVENT_SEEN_COUNT.fetch_add(1, Ordering::Relaxed) + 1;
                        let now = crate::utils::unix_now_ms();
                        let last = LAST_SETTINGS_EVENT_LOG_MS.load(Ordering::Relaxed);
                        if now.saturating_sub(last) >= 500
                            && LAST_SETTINGS_EVENT_LOG_MS
                                .compare_exchange(last, now, Ordering::Relaxed, Ordering::Relaxed)
                                .is_ok()
                        {
                            crate::log_ui!(
                                debug,
                                count = seen,
                                "PollingCoordinator saw settings event: {:?}",
                                event
                            );
                        }
                    });
                    if settings_events.saw_event {
                        needs_notify = true;
                    }
                    if settings_events.needs_reduce_motion_sync {
                        cx.update(|cx| {
                            cx.set_reduce_motion(
                                backend::settings::get_reduce_decorative_motion(),
                            );
                        });
                    }
                    if settings_events.needs_invalidation {
                        let _ = global_layout_cache::maybe_invalidate_for_settings_change();
                    }
                    if settings_events.needs_picker_refresh {
                        let _ = this.update(cx, |_coordinator, cx| {
                            if crate::components::overlays::refresh_open_emote_picker_popout(cx) {
                                crate::backend::metrics::record_ui_notify_source(
                                    "polling.settings.emote_picker_popout",
                                );
                            }
                        });
                    }
                }


                let _ = this.update(cx, |coordinator, cx| {
                    let removed = coordinator.overlay_manager.update(cx, |manager, cx| {
                        manager
                            .toast_manager
                            .update(cx, |toast_mgr, _cx| toast_mgr.remove_expired())
                    });
                    if removed {
                        needs_notify = true;
                    }
                });

                let should_recover_empty_channels =
                    poll_due(last_empty_channel_recovery, empty_channel_recovery_interval);
                let recovered_empty_channels = this
                    .update(cx, |coordinator, cx| {
                        let has_pending = coordinator
                            .app_state
                            .read(cx)
                            .chat_backend
                            .read(cx)
                            .has_pending_events();

                        let channels_key = coordinator
                            .app_state
                            .read(cx)
                            .active_tab()
                            .channels_change_key();
                        if !has_pending
                            && !should_recover_empty_channels
                            && tracked_channels_key == Some(channels_key)
                        {
                            return false;
                        }

                        // Forget channels the backend closed so a reopened
                        // channel is initialized again and neither set grows
                        // for the whole session.
                        {
                            let app_state = coordinator.app_state.read(cx);
                            let open = &app_state.chat_backend.read(cx).channels;
                            initialized_channels.retain(|channel| {
                                if channel
                                    .bytes()
                                    .any(|byte| byte.is_ascii_uppercase() || !byte.is_ascii())
                                {
                                    open.contains_key(&channel.to_lowercase())
                                } else {
                                    open.contains_key(channel.as_str())
                                }
                            });
                            tracked_channel_emote_versions
                                .retain(|channel, _| open.contains_key(channel));
                        }

                        let channels = coordinator.app_state.read(cx).active_tab().channels();

                        let has_new_channels =
                            channels.iter().any(|ch| !initialized_channels.contains(ch));

                        if !has_new_channels && !has_pending && !should_recover_empty_channels {
                            tracked_channels_key = Some(channels_key);
                            return false;
                        }

                        let new_channels: Vec<String> = if has_new_channels {
                            channels
                                .iter()
                                .filter(|ch| !initialized_channels.contains(*ch))
                                .cloned()
                                .collect()
                        } else {
                            Vec::new()
                        };

                        if !new_channels.is_empty() {
                            coordinator.app_state.update(cx, |state, cx| {
                                state.chat_backend.update(cx, |backend_entity, cx| {
                                    for channel in &new_channels {
                                        backend_entity.get_or_create_channel(channel, cx);
                                        initialized_channels.insert(channel.clone());
                                    }
                                });
                            });
                        }
                        tracked_channels_key = Some(channels_key);

                        if has_pending {
                            let processed = coordinator.app_state.update(cx, |state, cx| {
                                state.chat_backend.update(cx, |backend_entity, cx| {
                                    backend_entity.process_observer_events(cx)
                                })
                            });

                            if processed {
                                frame_rate_controller::record_activity();
                            }
                        }

                        let mut recovered_empty_channels = false;
                        if should_recover_empty_channels {
                            recovered_empty_channels =
                                coordinator.app_state.update(cx, |state, cx| {
                                    state.chat_backend.update(cx, |backend, cx| {
                                        backend.recover_empty_channels_from_backend(&channels, cx)
                                    })
                                });
                            if recovered_empty_channels {
                                frame_rate_controller::record_activity();
                            }
                        }

                        recovered_empty_channels
                    })
                    .unwrap_or(false);
                if should_recover_empty_channels {
                    last_empty_channel_recovery = Some(Instant::now());
                }
                if recovered_empty_channels {
                    frame_rate_controller::record_activity();
                    needs_notify = true;
                }

                if last_event_poll.elapsed() >= event_interval {
                    last_event_poll = Instant::now();
                    let events = backend::poll_twitch_events();
                    if !events.is_empty() {
                        frame_rate_controller::record_activity();
                        needs_notify = true;
                    }

                }

                if last_status_poll.elapsed() >= status_interval {
                    last_status_poll = Instant::now();
                    let status = backend::poll_connection_status();
                    let auth_cache_changed = refresh_auth_cache_if_changed();
                    if auth_cache_changed {
                        frame_rate_controller::record_activity();
                    }

                    let is_auth = backend::is_authenticated();
                    let auth_state = backend::get_auth_state();
                    let should_update_state = auth_cache_changed
                        || this
                            .update(cx, |coordinator, cx| {
                                let state = coordinator.app_state.read(cx);
                                state.connection_status != status
                                    || state.is_authenticated != is_auth
                                    || state.username != auth_state.username
                            })
                            .unwrap_or(true);

                    if should_update_state {
                        app_state.update(cx, |state, cx| {
                            state.connection_status = status;
                            state.is_authenticated = is_auth;
                            state.username = auth_state.username.clone();
                            cx.notify();
                        });
                    }
                }
                maybe_reload_custom_theme_file(
                    &mut last_theme_file_reload_check,
                    theme_file_reload_interval,
                );
                maybe_refresh_system_theme(
                    &mut last_system_theme_check,
                    system_theme_poll_interval,
                );

                if poll_due(last_stream_info, stream_info_interval) {
                    let (channels, last_hashes) = app_state
                        .update(cx, |state, _| stream_info_refresh_snapshot(state));
                    if backend::queue_stream_info_refresh(channels, last_hashes) {
                        last_stream_info = Some(Instant::now());
                    }
                }
                if let Some(updates) = backend::drain_pending_stream_info() {
                    app_state.update(cx, |state, state_cx| {
                        let applied = state.apply_stream_info_updates(updates);
                        if applied > 0 {
                            crate::log_ui!(debug, "stream info applied: {} channels", applied);
                            crate::backend::metrics::record_ui_notify_source(
                                "polling.stream_info",
                            );
                            state_cx.notify();
                        }
                    });
                }
                notify_live_uptime_rollover(&app_state, &mut last_uptime_fingerprint, cx);

                if last_sidebar_refresh.elapsed() >= sidebar_refresh_interval {
                    last_sidebar_refresh = Instant::now();
                    app_state.update(cx, |state, cx| {
                        if state.activity_open() {
                            let previous_signature = sidebar_cache_signature(state);
                            let scan_start = Instant::now();
                            state.refresh_sidebar_caches();
                            let scan_elapsed_us = scan_start.elapsed().as_micros() as u64;

                            SIDEBAR_REFRESH_LAST_US.store(scan_elapsed_us, Ordering::Relaxed);

                            let current_max = SIDEBAR_REFRESH_MAX_US.load(Ordering::Relaxed);
                            if scan_elapsed_us > current_max {
                                SIDEBAR_REFRESH_MAX_US.store(scan_elapsed_us, Ordering::Relaxed);
                                crate::log_perf!(
                                    debug,
                                    "Sidebar refresh scan new max: {}μs",
                                    scan_elapsed_us
                                );
                            }

                            let now_ms = crate::utils::unix_now_ms();
                            let last_log_ms = SIDEBAR_REFRESH_LOG_TIME_MS.load(Ordering::Relaxed);
                            if now_ms.saturating_sub(last_log_ms) >= 60_000 {
                                SIDEBAR_REFRESH_LOG_TIME_MS.store(now_ms, Ordering::Relaxed);
                                crate::log_perf!(
                                    debug,
                                    "Sidebar refresh: last={}μs, max={}μs",
                                    scan_elapsed_us,
                                    current_max.max(scan_elapsed_us)
                                );
                            }

                            if sidebar_cache_signature(state) != previous_signature {
                                crate::backend::metrics::record_ui_notify_source(
                                    "polling.sidebar_caches",
                                );
                                cx.notify();
                            }
                        }
                    });
                }

                if last_emote_poll.elapsed() >= emote_poll_interval {
                    last_emote_poll = Instant::now();

                    if let Some(ref emote_state_entity) = emote_state {
                        let current_global = backend::emotes::get_global_emote_version();
                        if current_global != tracked_global_emote_version {
                            let emote_count = backend::emotes::GLOBAL_EMOTES_LIST
                                .read()
                                .map(|list| list.len())
                                .unwrap_or(0);

                            crate::log_emotes!(
                                debug,
                                "Global emotes changed: {} -> {} ({} emotes)",
                                tracked_global_emote_version,
                                current_global,
                                emote_count
                            );
                            tracked_global_emote_version = current_global;

                            emote_state_entity.update(cx, |state, cx| {
                                state.notify_global_emotes_loaded(emote_count, cx);
                            });
                        }

                        let current_user = crate::state::VersionRegistry::user_emote_version();
                        if current_user != tracked_user_emote_version {
                            let emote_count = backend::emotes::USER_EMOTES_LOOKUP.load().len();

                            crate::log_emotes!(
                                debug,
                                "User emotes changed: {} -> {} ({} emotes)",
                                tracked_user_emote_version,
                                current_user,
                                emote_count
                            );
                            tracked_user_emote_version = current_user;

                            emote_state_entity.update(cx, |state, cx| {
                                state.notify_user_emotes_loaded(emote_count, cx);
                            });
                        }

                        if let Ok(channel_updates) = this.update(cx, |coordinator, cx| {
                            let active_channels: Vec<String> = coordinator
                                .app_state
                                .read(cx)
                                .active_tab()
                                .layout
                                .channels()
                                .into_iter()
                                .filter(|ch| !is_placeholder(ch))
                                .collect();

                            let mut updates = Vec::new();
                            for channel in active_channels {
                                let channel_lower = channel.to_lowercase();
                                let current_version =
                                    backend::emotes::get_emote_cache_version(&channel_lower);
                                updates.push((channel_lower, current_version));
                            }
                            updates
                        }) {
                            for (channel_lower, current_version) in channel_updates {
                                let tracked_version = tracked_channel_emote_versions
                                    .get(&channel_lower)
                                    .copied()
                                    .unwrap_or(0);

                                if current_version != tracked_version {
                                    let emote_count =
                                        backend::emotes::get_channel_emotes_fast(&channel_lower)
                                            .len();

                                    crate::log_emotes!(
                                        debug,
                                        "Channel '{}' emotes changed: {} -> {} ({} emotes)",
                                        channel_lower,
                                        tracked_version,
                                        current_version,
                                        emote_count
                                    );
                                    tracked_channel_emote_versions
                                        .insert(channel_lower.clone(), current_version);

                                    emote_state_entity.update(cx, |state, cx| {
                                        state.notify_channel_emotes_loaded(
                                            &channel_lower,
                                            emote_count,
                                            cx,
                                        );
                                    });
                                }
                            }
                        }
                    }
                }

                if last_image_cleanup.elapsed() >= image_cleanup_interval {
                    last_image_cleanup = Instant::now();
                    image_cache::sweep_idle_decoded_media();
                }

                crate::backend::memory_report::maybe_log_memory_breakdown();

                if last_layout_cleanup.elapsed() >= layout_cleanup_interval {
                    last_layout_cleanup = Instant::now();

                    backend::twitch_api::sweep_user_card_caches();

                    app_state.update(cx, |state, cx| {
                        state.cleanup_orphaned_states(cx);

                        state.rebalance_message_retention(cx);
                    });

                    let reclaimed_layouts =
                        crate::components::chat::canvas_renderer::global_layout_cache::drain_evicted_message_layouts();
                    if reclaimed_layouts > 0 {
                        crate::log_perf!(
                            debug,
                            "Reclaimed {} evicted-message layouts from global cache",
                            reclaimed_layouts
                        );
                    }

                    let cleaned = crate::backend::emote_candidates::cleanup_stale_pending(
                        |channel| crate::backend::emotes::EMOTE_CACHE.is_channel_loading(channel),
                    );
                    if cleaned > 0 {
                        crate::log_emotes!(
                            debug,
                            "Cleaned up {} stale pending emote channels",
                            cleaned
                        );
                    }

                    crate::backend::observers::get_observer_registry().cleanup_stale_observers();

                    crate::cache::frame_layer::cleanup_texture_cache();
                }


                let popout_requests = take_pending_popouts();
                for channel in popout_requests {
                    cx.update(|cx| {
                        crate::components::overlays::open_emote_popout(channel, cx);
                    });
                }

                if CLEAR_FILES_REFRESH_PENDING.swap(false, Ordering::AcqRel) {
                    cx.update(|cx| cx.refresh_windows());
                }

                if let Some(request) = crate::components::settings::drain_settings_popout_request()
                {
                    cx.update(|cx| {
                        crate::components::settings::open_settings_popout(request, cx);
                    });
                }

                if needs_notify {
                    app_state.update(cx, |_state, cx| {
                        crate::backend::metrics::record_ui_notify_source("polling.batched");
                        cx.notify();
                    });
                }
            }
        });

        self._task = Some(task);
    }
}

fn polling_allowed(network_lanes_suppressed: bool) -> bool {
    !network_lanes_suppressed
}

#[cfg(test)]
mod benchmark_tests {
    use super::*;
    use core::prelude::v1::test;

    #[test]
    fn offline_benchmark_suppresses_polling_coordinator() {
        assert!(!polling_allowed(true));
        assert!(polling_allowed(false));
    }
}
