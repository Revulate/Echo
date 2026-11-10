//! Moderation actions: timeout, ban, delete message
//! Routes through the Twitch Helix moderation endpoints, because Twitch IRC
//! does not accept chat commands (PRIVMSG "/timeout" etc.).

use super::commands::{CommandResult, helix};

fn command_result_to_unit(result: CommandResult) -> Result<(), String> {
    match result {
        CommandResult::Success(_) => Ok(()),
        CommandResult::Error(error) | CommandResult::DeliveryUnknown(error) => Err(error),
        CommandResult::NotACommand => Err("Moderation command was not executed".to_string()),
    }
}

pub(crate) async fn timeout_user_with_auth(
    channel: &str,
    username: &str,
    duration_seconds: u32,
    reason: Option<&str>,
    auth: &crate::backend::auth::AuthenticatedUserResource,
) -> Result<(), String> {
    command_result_to_unit(
        helix::cmd_timeout_username_with_auth(
            channel,
            username,
            u64::from(duration_seconds),
            reason,
            auth,
        )
        .await,
    )?;

    crate::log_irc!(
        info,
        "Timeout {} in {} for {}s",
        username,
        channel,
        duration_seconds
    );
    Ok(())
}

pub(crate) async fn ban_user_with_auth(
    channel: &str,
    username: &str,
    reason: Option<&str>,
    auth: &crate::backend::auth::AuthenticatedUserResource,
) -> Result<(), String> {
    command_result_to_unit(
        helix::cmd_ban_username_with_auth(channel, username, reason, auth).await,
    )?;

    crate::log_irc!(info, "Ban {} in {}", username, channel);
    Ok(())
}

pub(crate) async fn unban_user_with_auth(
    channel: &str,
    username: &str,
    auth: &crate::backend::auth::AuthenticatedUserResource,
) -> Result<(), String> {
    command_result_to_unit(helix::cmd_unban_username_with_auth(channel, username, auth).await)?;

    crate::log_irc!(info, "Unban {} in {}", username, channel);
    Ok(())
}

pub(crate) async fn clear_chat_with_auth(
    channel: &str,
    auth: &crate::backend::auth::AuthenticatedUserResource,
) -> Result<(), String> {
    command_result_to_unit(helix::delete_chat_messages_with_auth(channel, None, auth).await)?;

    crate::log_irc!(info, "Clear chat in {}", channel);
    Ok(())
}
