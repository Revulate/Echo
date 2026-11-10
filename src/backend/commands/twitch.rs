//! Twitch IRC-based commands
//! Commands that work by sending IRC messages through the connected client

use super::{CommandResult, helix, twitch_core};
use crate::backend::irc::{
    ensure_authenticated_send_ready, ensure_channel_send_ready, get_irc_client,
};
use twitch_irc::message::IRCMessage;

const DEFAULT_TIMEOUT_SECONDS: u64 = 600;

struct IdTarget<'a> {
    user_id: &'a str,
    rest: &'a str,
}

fn id_target<'a>(args: &'a str, usage: &str) -> Result<Option<IdTarget<'a>>, String> {
    let args = args.trim();
    let Some(first) = args.split_whitespace().next() else {
        return Ok(None);
    };
    let Some(user_id) = first.strip_prefix("id:") else {
        return Ok(None);
    };
    if user_id.is_empty() {
        return Err(usage.to_string());
    }

    let rest = args[first.len()..].trim_start();
    Ok(Some(IdTarget { user_id, rest }))
}

fn id_target_without_extra<'a>(args: &'a str, usage: &str) -> Result<Option<&'a str>, String> {
    let Some(target) = id_target(args, usage)? else {
        return Ok(None);
    };
    if !target.rest.is_empty() {
        return Err(usage.to_string());
    }
    Ok(Some(target.user_id))
}

/// Parse the `[duration] [reason]` tail shared by the username and id:userID
/// forms of /timeout.
fn parse_timeout_rest(rest: &str) -> Result<(u64, Option<&str>), String> {
    if rest.is_empty() {
        return Ok((DEFAULT_TIMEOUT_SECONDS, None));
    }

    let mut parts = rest.splitn(2, char::is_whitespace);
    let first = parts.next().unwrap_or_default();
    if let Some(duration) =
        twitch_core::parse_timeout_duration_token(first).map_err(str::to_string)?
    {
        let reason = parts
            .next()
            .map(|value| value.trim())
            .filter(|reason| !reason.is_empty());
        Ok((duration, reason))
    } else {
        Ok((DEFAULT_TIMEOUT_SECONDS, Some(rest)))
    }
}

#[allow(clippy::type_complexity)]
fn timeout_id_args(args: &str) -> Result<Option<(&str, u64, Option<&str>)>, String> {
    let Some(target) = id_target(
        args,
        "Usage: /timeout <username|id:userID> [duration] [reason]",
    )?
    else {
        return Ok(None);
    };
    let (duration, reason) = parse_timeout_rest(target.rest)?;
    Ok(Some((target.user_id, duration, reason)))
}

/// Send a raw IRC command to a channel
async fn send_irc(channel: &str, message: &str) -> CommandResult {
    if let Err(block) = ensure_authenticated_send_ready() {
        return CommandResult::Error(block.to_string());
    }
    let channel = match ensure_channel_send_ready(channel) {
        Ok(channel) => channel,
        Err(error) => return CommandResult::Error(error),
    };

    let client = match get_irc_client() {
        Some(c) => c,
        None => return CommandResult::Error("IRC client not connected".to_string()),
    };

    match client.privmsg(channel, message.to_string()).await {
        Ok(()) => CommandResult::Success(None),
        Err(e) => CommandResult::DeliveryUnknown(format!("Failed to send command: {e}")),
    }
}

pub async fn cmd_raw(args: &str) -> CommandResult {
    let raw = args.trim();
    if raw.is_empty() {
        return CommandResult::Error("Usage: /raw <irc message>".to_string());
    }
    if let Err(block) = ensure_authenticated_send_ready() {
        return CommandResult::Error(block.to_string());
    }

    let message = match IRCMessage::parse(raw) {
        Ok(message) => message,
        Err(error) => return CommandResult::Error(format!("Invalid raw IRC message: {error}")),
    };
    let client = match get_irc_client() {
        Some(c) => c,
        None => return CommandResult::Error("IRC client not connected".to_string()),
    };

    match client.send_message(message).await {
        Ok(()) => CommandResult::Success(None),
        Err(error) => {
            CommandResult::DeliveryUnknown(format!("Failed to send raw IRC message: {error}"))
        }
    }
}

pub async fn cmd_timeout(channel: &str, args: &str) -> CommandResult {
    let auth = match crate::backend::auth::capture_authenticated_user_resource() {
        Some(auth) => auth,
        None => return CommandResult::Error("Cannot run moderation commands until the active Twitch account identity finishes loading.".to_string()),
    };
    cmd_timeout_with_auth(channel, args, &auth).await
}

pub(crate) async fn cmd_timeout_with_auth(
    channel: &str,
    args: &str,
    auth: &crate::backend::auth::AuthenticatedUserResource,
) -> CommandResult {
    if args.is_empty() {
        return CommandResult::Error(
            "Usage: /timeout <username|id:userID> [duration] [reason]".to_string(),
        );
    }
    match timeout_id_args(args) {
        Ok(Some((user_id, duration, reason))) => {
            return helix::cmd_timeout_user_id_with_auth(channel, user_id, duration, reason, auth)
                .await;
        }
        Ok(None) => {}
        Err(usage) => return CommandResult::Error(usage),
    }

    let mut parts = args.splitn(2, char::is_whitespace);
    let username = parts.next().unwrap_or_default();
    let rest = parts.next().unwrap_or("").trim();
    let (duration, reason) = match parse_timeout_rest(rest) {
        Ok(parsed) => parsed,
        Err(error) => return CommandResult::Error(error),
    };
    helix::cmd_timeout_username_with_auth(channel, username, duration, reason, auth).await
}

pub async fn cmd_ban(channel: &str, args: &str) -> CommandResult {
    let auth = match crate::backend::auth::capture_authenticated_user_resource() {
        Some(auth) => auth,
        None => return CommandResult::Error("Cannot run moderation commands until the active Twitch account identity finishes loading.".to_string()),
    };
    cmd_ban_with_auth(channel, args, &auth).await
}

pub(crate) async fn cmd_ban_with_auth(
    channel: &str,
    args: &str,
    auth: &crate::backend::auth::AuthenticatedUserResource,
) -> CommandResult {
    if args.is_empty() {
        return CommandResult::Error("Usage: /ban <username|id:userID> [reason]".to_string());
    }
    match id_target(args, "Usage: /ban <username|id:userID> [reason]") {
        Ok(Some(target)) => {
            let reason = (!target.rest.is_empty()).then_some(target.rest);
            return helix::cmd_ban_user_id_with_auth(channel, target.user_id, reason, auth).await;
        }
        Ok(None) => {}
        Err(usage) => return CommandResult::Error(usage),
    }

    let mut parts = args.splitn(2, char::is_whitespace);
    let username = parts.next().unwrap_or_default();
    let reason = parts
        .next()
        .map(|value| value.trim())
        .filter(|reason| !reason.is_empty());
    helix::cmd_ban_username_with_auth(channel, username, reason, auth).await
}

pub async fn cmd_banid(channel: &str, args: &str) -> CommandResult {
    let auth = match crate::backend::auth::capture_authenticated_user_resource() {
        Some(auth) => auth,
        None => return CommandResult::Error("Cannot run moderation commands until the active Twitch account identity finishes loading.".to_string()),
    };
    cmd_banid_with_auth(channel, args, &auth).await
}

pub(crate) async fn cmd_banid_with_auth(
    channel: &str,
    args: &str,
    auth: &crate::backend::auth::AuthenticatedUserResource,
) -> CommandResult {
    match twitch_core::parse_banid_args(args) {
        Ok(user_id) => helix::cmd_ban_user_id_with_auth(channel, user_id, None, auth).await,
        Err(usage) => CommandResult::Error(usage.to_string()),
    }
}

pub async fn cmd_unban(channel: &str, args: &str) -> CommandResult {
    let auth = match crate::backend::auth::capture_authenticated_user_resource() {
        Some(auth) => auth,
        None => return CommandResult::Error("Cannot run moderation commands until the active Twitch account identity finishes loading.".to_string()),
    };
    cmd_unban_with_auth(channel, args, &auth).await
}

pub(crate) async fn cmd_unban_with_auth(
    channel: &str,
    args: &str,
    auth: &crate::backend::auth::AuthenticatedUserResource,
) -> CommandResult {
    unban_target_with_auth(channel, args, auth, "Usage: /unban <username|id:userID>").await
}

async fn unban_target_with_auth(
    channel: &str,
    args: &str,
    auth: &crate::backend::auth::AuthenticatedUserResource,
    usage: &'static str,
) -> CommandResult {
    if args.is_empty() {
        return CommandResult::Error(usage.to_string());
    }
    match id_target_without_extra(args, usage) {
        Ok(Some(user_id)) => {
            return helix::cmd_unban_user_id_with_auth(channel, user_id, auth).await;
        }
        Ok(None) => {}
        Err(usage) => return CommandResult::Error(usage),
    }
    match twitch_core::parse_single_username(args, usage) {
        Ok(username) => helix::cmd_unban_username_with_auth(channel, username, auth).await,
        Err(usage) => CommandResult::Error(usage.to_string()),
    }
}

pub async fn cmd_untimeout(channel: &str, args: &str) -> CommandResult {
    let auth = match crate::backend::auth::capture_authenticated_user_resource() {
        Some(auth) => auth,
        None => return CommandResult::Error("Cannot run moderation commands until the active Twitch account identity finishes loading.".to_string()),
    };
    cmd_untimeout_with_auth(channel, args, &auth).await
}

pub(crate) async fn cmd_untimeout_with_auth(
    channel: &str,
    args: &str,
    auth: &crate::backend::auth::AuthenticatedUserResource,
) -> CommandResult {
    unban_target_with_auth(
        channel,
        args,
        auth,
        "Usage: /untimeout <username|id:userID>",
    )
    .await
}

pub async fn cmd_delete(channel: &str, args: &str) -> CommandResult {
    let auth = match crate::backend::auth::capture_authenticated_user_resource() {
        Some(auth) => auth,
        None => return CommandResult::Error("Cannot run moderation commands until the active Twitch account identity finishes loading.".to_string()),
    };
    cmd_delete_with_auth(channel, args, &auth).await
}

pub(crate) async fn cmd_delete_with_auth(
    channel: &str,
    args: &str,
    auth: &crate::backend::auth::AuthenticatedUserResource,
) -> CommandResult {
    match twitch_core::parse_delete_args(args) {
        Ok(message_id) => {
            helix::delete_chat_messages_with_auth(channel, Some(message_id), auth).await
        }
        Err(usage) => CommandResult::Error(usage.to_string()),
    }
}

/// Pass through unknown commands to IRC (Twitch may handle them)
pub async fn cmd_passthrough(channel: &str, full_input: &str) -> CommandResult {
    send_irc(channel, full_input).await
}
