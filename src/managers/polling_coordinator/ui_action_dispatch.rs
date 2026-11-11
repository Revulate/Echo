//! Dispatching queued [`backend::UiAction`]s on the UI thread.
//!
//! [`dispatch_ui_actions`] routes each action to one handler. Actions that need
//! `EchoApp`-owned split or composer state are re-queued for `EchoApp` through
//! `backend::ui_actions`; everything else is handled here against the overlay
//! manager and app state.

use super::{open_external_link, spawn_clear_files};
use crate::backend;
use crate::backend::lock_utils::MutexExt;
use crate::components::overlays::ConfirmationModal;
use crate::managers::OverlayManager;
use crate::state::AppState;
use gpui::*;
use std::sync::Arc;

type ConfirmCallback = Arc<dyn Fn() + Send + Sync + 'static>;

pub(super) fn dispatch_ui_actions(
    app_state: &Entity<AppState>,
    overlay_manager: &Entity<OverlayManager>,
    cx: &mut AsyncApp,
) {
    let ui_actions = backend::poll_ui_actions();
    let needs_notify = !ui_actions.is_empty();
    for action in ui_actions {
        dispatch_ui_action(action, app_state, overlay_manager, cx);
    }
    if needs_notify {
        app_state.update(cx, |_state, cx| {
            crate::backend::metrics::record_ui_notify_source("polling.ui_actions");
            cx.notify();
        });
    }
}

fn dispatch_ui_action(
    action: backend::UiAction,
    app_state: &Entity<AppState>,
    overlay_manager: &Entity<OverlayManager>,
    cx: &mut AsyncApp,
) {
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
            let state = crate::components::overlays::UserCardState {
                is_open: true,
                username: Some(username),
                display_name: Some(display_name),
                user_id,
                channel: Some(channel),
                color,
                badges,
                x,
                y,
            };
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
            refresh_chatter_list(channel, source, overlay_manager, cx);
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
                source: Some(source),
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
                    list.set_load_error(&channel, source, error.clone(), cx);
                });
            });
        }
        backend::UiAction::RemoveChatterListEntry {
            channel,
            source,
            username,
        } => {
            remove_chatter_list_entry(&channel, source, &username, overlay_manager, cx);
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
            backend::push_pending_message_composer_insert(channel, source_split_id, text, quote);
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
                refresh_sidebar_caches(app_state, cx);
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
                refresh_sidebar_caches(app_state, cx);
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
            confirm_open_link(url, mode, overlay_manager, cx);
        }
        backend::UiAction::ConfirmChatterinoTabReplace { tab_count } => {
            let on_confirm: ConfirmCallback =
                Arc::new(backend::chatterino_import::confirm_pending_replace);
            let on_cancel: ConfirmCallback =
                Arc::new(backend::chatterino_import::cancel_pending_replace);
            let tabs = if tab_count == 1 { "tab" } else { "tabs" };
            open_confirmation_with(
                overlay_manager,
                cx,
                "Replace your tabs?".to_string(),
                format!(
                    "Echo will close your open tabs and show the {tab_count} imported {tabs}. Your tabs are backed up first, and Reopen Closed Tab brings them back until Echo closes."
                ),
                "Replace tabs",
                on_confirm,
                |modal| modal.on_cancel = Some(on_cancel),
            );
        }
        backend::UiAction::ConfirmCloseTabs {
            tab_id,
            to_right,
            count,
        } => {
            confirm_close_tabs(tab_id, to_right, count, overlay_manager, cx);
        }
        backend::UiAction::ConfirmClearUploadHistory => {
            open_confirmation(
                overlay_manager,
                cx,
                "Clear upload history?".to_string(),
                "This forgets Echo's local upload records. It does not delete files from their hosting providers.".to_string(),
                "Clear history",
                Arc::new(|| {
                    let removed = backend::upload_history::clear_history();
                    backend::push_ui_action(backend::UiAction::ShowSuccessToast {
                        message: "Upload history cleared".to_string(),
                        detail: Some(format!("Forgot {removed} local upload records")),
                    });
                }),
            );
        }
        backend::UiAction::ConfirmClearReadableLogs => {
            open_confirmation(
                overlay_manager,
                cx,
                "Clear readable channel logs?".to_string(),
                "Echo will delete the daily chat log files it wrote in your log folder. Other files in that folder are left alone.".to_string(),
                "Delete logs",
                Arc::new(|| {
                    spawn_clear_files(
                        backend::history_cache::clear_readable_logs,
                        "Readable channel logs cleared",
                        "log",
                        "Failed to clear readable channel logs",
                    );
                }),
            );
        }
        backend::UiAction::ConfirmClearHistoryExports => {
            open_confirmation(
                overlay_manager,
                cx,
                "Clear history text exports?".to_string(),
                "Echo will delete the history text exports it wrote to its exports folder."
                    .to_string(),
                "Delete exports",
                Arc::new(|| {
                    spawn_clear_files(
                        backend::history_cache::clear_history_text_exports,
                        "History text exports cleared",
                        "export",
                        "Failed to clear history text exports",
                    );
                }),
            );
        }
        backend::UiAction::ConfirmResetHotkeys => {
            open_confirmation(
                overlay_manager,
                cx,
                "Reset all hotkeys?".to_string(),
                "Every built-in hotkey goes back to its default binding. Custom command hotkeys are kept.".to_string(),
                "Reset hotkeys",
                Arc::new(|| {
                    backend::hotkeys::reset_all_bindings();
                }),
            );
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
                backend::push_ui_action(backend::UiAction::UploaderMappingConfirmationResolved {
                    origin: origin.clone(),
                    origin_window,
                    applied,
                    status_revision,
                });
            });
            let on_cancel = std::sync::Arc::new(move || {
                backend::push_ui_action(backend::UiAction::UploaderMappingConfirmationResolved {
                    origin: cancel_origin.clone(),
                    origin_window,
                    applied: false,
                    status_revision,
                });
            });
            open_confirmation_with(
                overlay_manager,
                cx,
                "Replace your saved uploader mapping?".to_string(),
                "Your saved request and response mapping will be replaced. Importing a preset without an Echo extension may also enable uploads, paste uploads, and drag-and-drop uploads. Cancel keeps the current mapping.".to_string(),
                "Replace mapping",
                on_confirm,
                |modal| {
                    modal.uploader_owner = Some(cancel_origin_host);
                    modal.on_cancel = Some(on_cancel);
                },
            );
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
            open_confirmation(
                overlay_manager,
                cx,
                "Reset moderation buttons?".to_string(),
                "Your moderation buttons will be replaced with the default set.".to_string(),
                "Reset buttons",
                Arc::new(|| {
                    crate::components::settings::reset_mod_buttons_to_defaults();
                }),
            );
        }
        backend::UiAction::ConfirmClearMutedSounds => {
            open_confirmation(
                overlay_manager,
                cx,
                "Unmute all channels?".to_string(),
                "Sounds and desktop notifications will be allowed again in every channel you muted."
                    .to_string(),
                "Unmute all",
                Arc::new(|| {
                    backend::clear_highlight_sound_muted_channels();
                }),
            );
        }
        backend::UiAction::ConfirmClearMessageCache => {
            open_confirmation(
                overlay_manager,
                cx,
                "Clear message cache?".to_string(),
                "Echo will delete the chat history it saved on disk for every channel.".to_string(),
                "Clear cache",
                Arc::new(|| {
                    crate::components::settings::clear_message_cache();
                }),
            );
        }
        backend::UiAction::ConfirmClearModLog => {
            open_confirmation(
                overlay_manager,
                cx,
                "Clear moderation log?".to_string(),
                "All moderation actions recorded this session will be removed from the log."
                    .to_string(),
                "Clear log",
                Arc::new(|| {
                    backend::bridge::clear_mod_actions();
                }),
            );
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
            confirm_remove_account(username, display_name, overlay_manager, cx);
        }
        backend::UiAction::ConfirmBlockUser {
            user_id,
            username,
            display_name,
            expected_owner_user_id,
        } => {
            confirm_block_user(
                &user_id,
                &username,
                &display_name,
                &expected_owner_user_id,
                overlay_manager,
                cx,
            );
        }
        backend::UiAction::ConfirmResetSettings => {
            open_confirmation(
                overlay_manager,
                cx,
                "Reset all settings?".to_string(),
                "Echo will restore default settings and keep your current Twitch accounts. A backup is created before the reset.".to_string(),
                "Reset",
                Arc::new(reset_settings_to_defaults),
            );
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
            follow_watching_channel(channel.as_deref(), app_state, cx);
        }
        backend::UiAction::ShowUpdateAvailableToast { version } => {
            use crate::components::overlays::Toast;

            let toast = Toast::info(format!("Echo {version} is available"))
                .with_detail("Click to view the signed update and changelog")
                .with_duration(10)
                .with_on_click(|| {
                    backend::push_ui_action(backend::UiAction::OpenUpdatesSettings);
                });
            push_toast(overlay_manager, toast, cx);
        }
        backend::UiAction::ShowSuccessToast { message, detail } => {
            let toast = crate::components::overlays::Toast::success(message);
            push_toast(overlay_manager, with_optional_detail(toast, detail), cx);
        }
        backend::UiAction::ShowWarningToast { message, detail } => {
            let toast = crate::components::overlays::Toast::warning(message);
            push_toast(overlay_manager, with_optional_detail(toast, detail), cx);
        }
        backend::UiAction::ShowEmoteDisplayRuleToast { message, undo } => {
            let toast =
                crate::components::overlays::Toast::info(message).with_action("Undo", move || {
                    backend::settings::apply_emote_display_rule_changes(&undo);
                });
            push_toast(overlay_manager, toast, cx);
        }
        backend::UiAction::ExplainHighlight { highlight_type } => {
            show_highlight_explanation(highlight_type, overlay_manager, cx);
        }
        backend::UiAction::EditHighlightRule { highlight_type } => {
            edit_highlight_rule(&highlight_type, overlay_manager, cx);
        }
    }
}

fn refresh_sidebar_caches(app_state: &Entity<AppState>, cx: &mut AsyncApp) {
    app_state.update(cx, |state, cx| {
        state.refresh_sidebar_caches();
        cx.notify();
    });
}

fn push_toast(
    overlay_manager: &Entity<OverlayManager>,
    toast: crate::components::overlays::Toast,
    cx: &mut AsyncApp,
) {
    overlay_manager.update(cx, |manager, cx| {
        manager.push_toast(toast, cx);
    });
}

fn with_optional_detail(
    toast: crate::components::overlays::Toast,
    detail: Option<String>,
) -> crate::components::overlays::Toast {
    match detail {
        Some(detail) => toast.with_detail(detail),
        None => toast,
    }
}

/// Open the shared confirmation modal for a destructive action.
fn open_confirmation(
    overlay_manager: &Entity<OverlayManager>,
    cx: &mut AsyncApp,
    title: String,
    message: String,
    confirm_text: &str,
    on_confirm: ConfirmCallback,
) {
    open_confirmation_with(
        overlay_manager,
        cx,
        title,
        message,
        confirm_text,
        on_confirm,
        |_| {},
    );
}

/// [`open_confirmation`], then let `configure` adjust the opened modal.
fn open_confirmation_with(
    overlay_manager: &Entity<OverlayManager>,
    cx: &mut AsyncApp,
    title: String,
    message: String,
    confirm_text: &str,
    on_confirm: ConfirmCallback,
    configure: impl FnOnce(&mut ConfirmationModal),
) {
    overlay_manager.update(cx, |manager, cx| {
        manager.confirmation_modal.update(cx, |modal, cx| {
            modal.open_simple(
                title,
                message,
                confirm_text.to_string(),
                true,
                on_confirm,
                cx,
            );
            configure(modal);
        });
    });
}

fn refresh_chatter_list(
    channel: String,
    source: Option<backend::ui_actions::ChatterListSource>,
    overlay_manager: &Entity<OverlayManager>,
    cx: &mut AsyncApp,
) {
    overlay_manager.update(cx, |manager, cx| {
        manager.open_chatter_list_loading(channel.clone(), cx);
    });
    crate::managers::frame_rate_controller::record_activity();

    crate::backend::bridge::get_runtime().spawn(async move {
        match crate::backend::commands::helix::refresh_chatter_list_source(&channel, source).await {
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
                    source.map_or("", |source| source.label()),
                    error
                );
                crate::backend::push_ui_action(crate::backend::UiAction::ChatterListLoadFailed {
                    channel: channel.clone(),
                    source,
                    error: error.clone(),
                });
                crate::backend::push_ui_action(crate::backend::UiAction::ShowWarningToast {
                    message: "Failed to refresh chatter list".to_string(),
                    detail: Some(error),
                });
            }
            crate::backend::commands::CommandResult::NotACommand => {}
        }
        crate::managers::frame_rate_controller::record_activity();
    });
}

fn remove_chatter_list_entry(
    channel: &str,
    source: backend::ui_actions::ChatterListSource,
    username: &str,
    overlay_manager: &Entity<OverlayManager>,
    cx: &mut AsyncApp,
) {
    overlay_manager.update(cx, |manager, cx| {
        manager.chatter_list.update(cx, |list, cx| {
            if list.state.channel.eq_ignore_ascii_case(channel) && list.state.source == Some(source)
            {
                let before = list.state.chatters.len();
                std::sync::Arc::make_mut(&mut list.state.chatters)
                    .retain(|chatter| !chatter.username.eq_ignore_ascii_case(username));
                if list.state.chatters.len() != before {
                    list.state.total_count = list.state.total_count.saturating_sub(1);
                    list.rebuild_visible_indices();
                    cx.notify();
                }
            }
        });
    });
}

fn confirm_open_link(
    url: String,
    mode: backend::ui_actions::LinkOpenMode,
    overlay_manager: &Entity<OverlayManager>,
    cx: &mut AsyncApp,
) {
    if !backend::settings::should_confirm_open_link(&url) {
        open_external_link(&url, mode);
        return;
    }

    let mode_label = match mode {
        backend::ui_actions::LinkOpenMode::Configured => "configured browser mode",
        backend::ui_actions::LinkOpenMode::Normal => "normal browser window",
        backend::ui_actions::LinkOpenMode::Incognito => "private browser window",
    };
    let message = format!("Echo will open this external link using {mode_label}:\n{url}");

    let host = backend::settings::external_link_host(&url);
    let url_for_confirm = url.clone();
    let on_confirm = std::sync::Arc::new(move || open_external_link(&url_for_confirm, mode));
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

fn confirm_close_tabs(
    tab_id: String,
    to_right: bool,
    count: usize,
    overlay_manager: &Entity<OverlayManager>,
    cx: &mut AsyncApp,
) {
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
            "Echo will close every tab except this one. Reopen Closed Tab brings them back."
                .to_string(),
        )
    };
    open_confirmation(
        overlay_manager,
        cx,
        title,
        message,
        "Close tabs",
        on_confirm,
    );
}

fn confirm_remove_account(
    username: String,
    display_name: String,
    overlay_manager: &Entity<OverlayManager>,
    cx: &mut AsyncApp,
) {
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
    open_confirmation(overlay_manager, cx, title, message, "Remove", on_confirm);
}

fn confirm_block_user(
    user_id: &str,
    username: &str,
    display_name: &str,
    expected_owner_user_id: &str,
    overlay_manager: &Entity<OverlayManager>,
    cx: &mut AsyncApp,
) {
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
            return;
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
        return;
    }
    if backend::blocked_users::is_blocked(Some(&user.user_id), &user.user_login) {
        backend::push_ui_action(backend::UiAction::ShowSuccessToast {
            message: "User is already blocked on Twitch".to_string(),
            detail: None,
        });
        return;
    }

    let on_confirm = std::sync::Arc::new(move || {
        backend::blocked_users::set_block_state_async_for_session(
            session.clone(),
            user.clone(),
            true,
        );
    });
    open_confirmation(
        overlay_manager,
        cx,
        "Block Twitch user?".to_string(),
        "Echo will block this Twitch user on the active account. Their chat messages and whispers will be hidden according to your blocked-user settings.".to_string(),
        "Block",
        on_confirm,
    );
}

fn reset_settings_to_defaults() {
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
}

/// The channel Watching splits followed as of the last change, kept so the
/// next change can release it.
static LAST_FOLLOWED_WATCHING_CHANNEL: std::sync::Mutex<Option<String>> =
    std::sync::Mutex::new(None);

fn follow_watching_channel(channel: Option<&str>, app_state: &Entity<AppState>, cx: &mut AsyncApp) {
    // The channel a Watching split showed until now has left the layout
    // without any split being closed, so nothing else releases it.
    let previous = std::mem::replace(
        &mut *LAST_FOLLOWED_WATCHING_CHANNEL.lock_or_recover(),
        channel.map(str::to_string),
    );
    cx.update(|cx| {
        app_state.update(cx, |state, cx| match previous.as_deref() {
            Some(previous) if Some(previous) != channel => {
                state.release_closed_channel(previous, cx)
            }
            _ => state.cleanup_orphaned_states(cx),
        })
    });
    if let Some(channel) = channel {
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

fn show_highlight_explanation(
    highlight_type: crate::backend::HighlightType,
    overlay_manager: &Entity<OverlayManager>,
    cx: &mut AsyncApp,
) {
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
    push_toast(overlay_manager, toast, cx);
}

fn edit_highlight_rule(
    highlight_type: &crate::backend::HighlightType,
    overlay_manager: &Entity<OverlayManager>,
    cx: &mut AsyncApp,
) {
    use crate::components::overlays::Toast;

    let rule = echo_core::highlight_explanation::highlight_rule_origin(highlight_type)
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
