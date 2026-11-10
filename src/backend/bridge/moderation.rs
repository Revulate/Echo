//! Moderation actions
//!
//! Handles timeouts, bans, unbans, message deletion, and mod log.

use super::{ModAction, RUNTIME};
use crate::backend::commands::CommandResult;
use crate::backend::components::mod_log::record_local_mod_action;

fn push_success_toast(message: String) {
    crate::backend::push_ui_action(crate::backend::UiAction::ShowSuccessToast {
        message,
        detail: None,
    });
}

fn publish_warning(channel: &str, message: String, detail: String) {
    super::chat::publish_command_error(channel, &message, &detail);
}

fn should_hide_mod_details() -> bool {
    crate::backend::streamer_mode::should_hide_mod_actions()
}

pub(crate) fn push_moderation_success_toast(visible: String, hidden: &'static str) {
    if should_hide_mod_details() {
        push_success_toast(hidden.to_string());
    } else {
        push_success_toast(visible);
    }
}

fn publish_moderation_warning(channel: &str, visible_message: String, error: String) {
    if should_hide_mod_details() {
        publish_warning(
            channel,
            "Moderation action failed".to_string(),
            "Moderation details hidden in streamer mode.".to_string(),
        );
    } else {
        publish_warning(channel, visible_message, error);
    }
}

/// Direct moderation controls bypass text-command dispatch, so they must take
/// the live benchmark's no-send gate before they capture auth or spawn a
/// remote Helix request.
fn reject_live_moderation_action(channel: &str, action: &str) -> bool {
    match crate::benchmark_profile::reject_live_outbound_user_action(action) {
        Ok(()) => false,
        Err(error) => {
            crate::log_irc!(warn, "{error}");
            publish_moderation_warning(
                channel,
                "Live benchmark blocked moderation action".to_string(),
                error,
            );
            true
        }
    }
}

/// Preflight failures happen before a remote request exists, so they remain
/// safe to show immediately. Only post-await completions use the account fence.
fn capture_ui_moderation_auth(
    channel: &str,
) -> Option<crate::backend::auth::AuthenticatedUserResource> {
    crate::backend::auth::capture_authenticated_user_resource().or_else(|| {
        publish_moderation_warning(channel,
            "Moderation action unavailable".to_string(),
            "The active Twitch account identity is still loading. Try again once authentication finishes."
                .to_string(),
        );
        None
    })
}

fn duration_label(duration_seconds: u32) -> String {
    match duration_seconds {
        1 => "1 second".to_string(),
        seconds if seconds < 60 => format!("{seconds} seconds"),
        seconds if seconds % 86_400 == 0 => {
            let days = seconds / 86_400;
            if days == 1 {
                "1 day".to_string()
            } else {
                format!("{days} days")
            }
        }
        seconds if seconds % 3_600 == 0 => {
            let hours = seconds / 3_600;
            if hours == 1 {
                "1 hour".to_string()
            } else {
                format!("{hours} hours")
            }
        }
        seconds if seconds % 60 == 0 => {
            let minutes = seconds / 60;
            if minutes == 1 {
                "1 minute".to_string()
            } else {
                format!("{minutes} minutes")
            }
        }
        seconds => format!("{seconds} seconds"),
    }
}

fn handle_role_command_result(
    auth: &crate::backend::auth::AuthenticatedUserResource,
    result: CommandResult,
    mutation: RoleMutation,
    username: &str,
    channel: &str,
    chatter_list_source_to_remove: Option<&'static str>,
) {
    let published = auth.commit_if_current(|| match result {
        CommandResult::Success(message) => {
            let detail = message.unwrap_or_else(|| mutation.success_message(username, channel));
            record_local_mod_action(
                mutation.action_type(),
                username.to_string(),
                channel.to_string(),
                detail.clone(),
            );
            if let Some(source) = chatter_list_source_to_remove {
                crate::backend::push_ui_action(crate::backend::UiAction::RemoveChatterListEntry {
                    channel: channel.to_string(),
                    source: source.to_string(),
                    username: username.to_string(),
                });
            }
            push_moderation_success_toast(detail, mutation.hidden_success_message());
        }
        CommandResult::Error(error) | CommandResult::DeliveryUnknown(error) => {
            publish_moderation_warning(channel, "Moderation action failed".to_string(), error)
        }
        CommandResult::NotACommand => publish_warning(
            channel,
            "Moderation action failed".to_string(),
            "Echo did not execute a moderation command".to_string(),
        ),
    });
    if published.is_none() {
        crate::log_auth!(
            debug,
            "Discarded stale role-action UI publication; the prior-account request may have applied"
        );
    }
}

/// Publish all local moderation effects through the account fence. Completion
/// follows a remote request, so an account change makes the previous request's
/// final effect unknown; never show that result in the new account's UI.
fn publish_remote_moderation_result(
    auth: &crate::backend::auth::AuthenticatedUserResource,
    result: Result<(), String>,
    on_success: impl FnOnce(),
    on_error: impl FnOnce(String),
) {
    if auth
        .commit_if_current(|| match result {
            Ok(()) => on_success(),
            Err(error) => on_error(error),
        })
        .is_none()
    {
        crate::log_auth!(
            debug,
            "Suppressed stale remote moderation completion; the prior-account request may have applied"
        );
    }
}

/// Timeout a user
pub fn timeout_user(channel: &str, username: &str, duration_seconds: u32, reason: Option<&str>) {
    if reject_live_moderation_action(channel, "moderation timeout") {
        return;
    }
    let channel = channel.to_string();
    let username = username.to_string();
    let reason = reason.map(|s| s.to_string());
    let Some(auth) = capture_ui_moderation_auth(&channel) else {
        return;
    };
    RUNTIME.spawn(async move {
        let result = crate::backend::moderation::timeout_user_with_auth(
            &channel,
            &username,
            duration_seconds,
            reason.as_deref(),
            &auth,
        )
        .await;
        publish_remote_moderation_result(
            &auth,
            result,
            || {
                let reason_suffix = reason
                    .as_deref()
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(|value| format!(" ({value})"))
                    .unwrap_or_default();
                let detail = format!(
                    "Timed out {} in #{} for {}{}",
                    username,
                    channel,
                    duration_label(duration_seconds),
                    reason_suffix
                );
                record_local_mod_action(
                    crate::backend::components::mod_log::ModActionType::Timeout,
                    username.clone(),
                    channel.clone(),
                    detail.clone(),
                );
                push_moderation_success_toast(detail, "Timed out a user.");
            },
            |e| {
                crate::log_irc!(warn, "Timeout failed: {}", e);
                publish_moderation_warning(
                    &channel,
                    format!("Failed to timeout {} in #{}", username, channel),
                    e,
                );
            },
        );
    });
}

/// Ban a user
pub fn ban_user(channel: &str, username: &str, reason: Option<&str>) {
    if reject_live_moderation_action(channel, "moderation ban") {
        return;
    }
    let channel = channel.to_string();
    let username = username.to_string();
    let reason = reason.map(|s| s.to_string());
    let Some(auth) = capture_ui_moderation_auth(&channel) else {
        return;
    };
    RUNTIME.spawn(async move {
        let result = crate::backend::moderation::ban_user_with_auth(
            &channel,
            &username,
            reason.as_deref(),
            &auth,
        )
        .await;
        publish_remote_moderation_result(
            &auth,
            result,
            || {
                let reason_suffix = reason
                    .as_deref()
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(|value| format!(" ({value})"))
                    .unwrap_or_default();
                let detail = format!("Banned {} in #{}{}", username, channel, reason_suffix);
                record_local_mod_action(
                    crate::backend::components::mod_log::ModActionType::Ban,
                    username.clone(),
                    channel.clone(),
                    detail.clone(),
                );
                push_moderation_success_toast(detail, "Banned a user.");
            },
            |e| {
                crate::log_irc!(warn, "Ban failed: {}", e);
                publish_moderation_warning(
                    &channel,
                    format!("Failed to ban {} in #{}", username, channel),
                    e,
                );
            },
        );
    });
}

/// Unban a user
pub fn unban_user(channel: &str, username: &str) {
    if reject_live_moderation_action(channel, "moderation unban") {
        return;
    }
    let channel = channel.to_string();
    let username = username.to_string();
    let Some(auth) = capture_ui_moderation_auth(&channel) else {
        return;
    };
    RUNTIME.spawn(async move {
        let result =
            crate::backend::moderation::unban_user_with_auth(&channel, &username, &auth).await;
        publish_remote_moderation_result(
            &auth,
            result,
            || {
                let detail = format!("Removed ban/timeout for {} in #{}", username, channel);
                record_local_mod_action(
                    crate::backend::components::mod_log::ModActionType::Unban,
                    username.clone(),
                    channel.clone(),
                    detail.clone(),
                );
                push_moderation_success_toast(detail, "Removed a ban or timeout.");
            },
            |e| {
                crate::log_irc!(warn, "Unban failed: {}", e);
                publish_moderation_warning(
                    &channel,
                    format!(
                        "Failed to remove ban/timeout for {} in #{}",
                        username, channel
                    ),
                    e,
                );
            },
        );
    });
}

/// Clear chat
pub fn clear_chat(channel: &str) {
    if reject_live_moderation_action(channel, "moderation clear") {
        return;
    }
    let channel = channel.to_string();
    let Some(auth) = capture_ui_moderation_auth(&channel) else {
        return;
    };
    RUNTIME.spawn(async move {
        let result = crate::backend::moderation::clear_chat_with_auth(&channel, &auth).await;
        publish_remote_moderation_result(
            &auth,
            result,
            || {
                let detail = format!("Cleared chat in #{}", channel);
                record_local_mod_action(
                    crate::backend::components::mod_log::ModActionType::Clear,
                    "moderator".to_string(),
                    channel.clone(),
                    detail.clone(),
                );
                push_moderation_success_toast(detail, "Cleared chat.");
            },
            |e| {
                crate::log_irc!(warn, "Clear chat failed: {}", e);
                publish_moderation_warning(
                    &channel,
                    format!("Failed to clear chat in #{}", channel),
                    e,
                );
            },
        );
    });
}

#[derive(Clone, Copy)]
enum RoleMutation {
    Mod,
    Unmod,
    Vip,
    Unvip,
}

impl RoleMutation {
    fn action_type(self) -> crate::backend::components::mod_log::ModActionType {
        match self {
            Self::Mod => crate::backend::components::mod_log::ModActionType::Mod,
            Self::Unmod => crate::backend::components::mod_log::ModActionType::Unmod,
            Self::Vip => crate::backend::components::mod_log::ModActionType::Vip,
            Self::Unvip => crate::backend::components::mod_log::ModActionType::Unvip,
        }
    }

    fn success_message(self, username: &str, channel: &str) -> String {
        match self {
            Self::Mod => format!("Added {username} as a moderator in #{channel}"),
            Self::Unmod => format!("Removed {username} as a moderator in #{channel}"),
            Self::Vip => format!("Added {username} as a VIP in #{channel}"),
            Self::Unvip => format!("Removed {username} as a VIP in #{channel}"),
        }
    }

    fn hidden_success_message(self) -> &'static str {
        match self {
            Self::Mod => "Added a moderator.",
            Self::Unmod => "Removed a moderator.",
            Self::Vip => "Added a VIP.",
            Self::Unvip => "Removed a VIP.",
        }
    }

    async fn execute(
        self,
        channel: &str,
        username: &str,
        auth: &crate::backend::auth::AuthenticatedUserResource,
    ) -> CommandResult {
        match self {
            Self::Mod => {
                crate::backend::commands::helix::cmd_mod_with_auth(channel, username, auth).await
            }
            Self::Unmod => {
                crate::backend::commands::helix::cmd_unmod_with_auth(channel, username, auth).await
            }
            Self::Vip => {
                crate::backend::commands::helix::cmd_vip_with_auth(channel, username, auth).await
            }
            Self::Unvip => {
                crate::backend::commands::helix::cmd_unvip_with_auth(channel, username, auth).await
            }
        }
    }
}

fn run_role_mutation(
    channel: &str,
    username: &str,
    mutation: RoleMutation,
    chatter_list_source_to_remove: Option<&'static str>,
) {
    if reject_live_moderation_action(channel, "moderation role change") {
        return;
    }
    let channel = channel.to_string();
    let username = username.to_string();
    let Some(auth) = capture_ui_moderation_auth(&channel) else {
        return;
    };
    RUNTIME.spawn(async move {
        if !auth.is_current() {
            return;
        }
        let result = mutation.execute(&channel, &username, &auth).await;
        handle_role_command_result(
            &auth,
            result,
            mutation,
            &username,
            &channel,
            chatter_list_source_to_remove,
        );
    });
}

pub fn mod_user(channel: &str, username: &str) {
    run_role_mutation(channel, username, RoleMutation::Mod, None);
}

pub fn unmod_user(channel: &str, username: &str) {
    run_role_mutation(channel, username, RoleMutation::Unmod, None);
}

pub fn vip_user(channel: &str, username: &str) {
    run_role_mutation(channel, username, RoleMutation::Vip, None);
}

pub fn unvip_user(channel: &str, username: &str) {
    run_role_mutation(channel, username, RoleMutation::Unvip, None);
}

pub fn unmod_chatter_list_user(channel: &str, username: &str) {
    run_role_mutation(
        channel,
        username,
        RoleMutation::Unmod,
        Some("Twitch moderators"),
    );
}

pub fn unvip_chatter_list_user(channel: &str, username: &str) {
    run_role_mutation(channel, username, RoleMutation::Unvip, Some("Twitch VIPs"));
}

/// Get recent mod actions
pub fn get_mod_actions(limit: u32) -> Vec<ModAction> {
    crate::backend::components::mod_log::get_mod_actions()
        .into_iter()
        .take(limit as usize)
        .map(|a| a.into())
        .collect()
}

/// Clear all mod actions
pub fn clear_mod_actions() {
    crate::backend::components::mod_log::clear_mod_actions();
}
