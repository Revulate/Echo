//! Twitch Helix API commands
//! Commands that require HTTP API calls rather than IRC messages

use super::{CommandFailure, CommandResult};
use crate::backend::chatters_core::{
    CHATTERS_PAGE_SIZE, HelixChatter, build_chatters_url, format_chatters_status,
    normalize_chatters, parse_chatters_page,
};
use crate::backend::constants::{HELIX_WHISPERS_ENDPOINT, TWITCH_WHISPER_MANAGE_SCOPE};
use crate::backend::settings::{get_token, get_username};
use crate::backend::twitch_api::{
    HelixAuthExt, TWITCH_HTTP_CLIENT, get_cached_channel_id, get_user_id_cached,
    renew_helix_session, send_helix_idempotent,
};
use crate::backend::ui_actions::ChatterListEntry;
use serde::Deserialize;
use std::collections::HashSet;
use url::Url;

/// Resolve the current user's Twitch ID by looking up the stored username
async fn get_current_user_id() -> Result<String, String> {
    let username = get_username().ok_or("Not authenticated (no username)")?;
    get_user_id_cached(&username)
        .await
        .map_err(|e| format!("Could not resolve own user ID: {e}"))
}

const WHISPER_ERROR_MAX_CHARS: usize = 512;

#[derive(Deserialize)]
struct HelixWhisperErrorPayload {
    #[serde(default)]
    message: String,
}

#[derive(Deserialize)]
struct HelixWhisperRecipientLookup {
    #[serde(default)]
    data: Vec<HelixWhisperRecipient>,
}

#[derive(Deserialize)]
struct HelixWhisperRecipient {
    id: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WhisperRecipientLookupError {
    InvalidLogin,
    NotFound,
    Authentication,
    Permission,
    RateLimited,
    ServiceUnavailable,
    Transport,
    InvalidResponse,
    UnexpectedResponse,
}

#[derive(Clone, Debug)]
pub(crate) struct WhisperContractSnapshot {
    pub endpoint: &'static str,
    pub scope: &'static str,
    pub scope_requested: bool,
    pub request_exact: bool,
    pub login_validation: bool,
    pub api_error_cases: usize,
    pub errors_mapped: bool,
    pub sanitizer_bounded: bool,
}

fn normalize_whisper_target(target: &str) -> Option<String> {
    let login = target.trim().trim_start_matches('@').to_ascii_lowercase();
    (!login.is_empty()
        && login.len() <= 25
        && login
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_'))
    .then_some(login)
}

fn format_whisper_recipient_lookup_error(error: WhisperRecipientLookupError) -> String {
    match error {
        WhisperRecipientLookupError::InvalidLogin => {
            "Enter a valid Twitch username before sending a whisper.".to_string()
        }
        WhisperRecipientLookupError::NotFound => "No user matching that username.".to_string(),
        WhisperRecipientLookupError::Authentication => {
            "Failed to send whisper - Twitch rejected the active account. Re-authenticate and try again."
                .to_string()
        }
        WhisperRecipientLookupError::Permission => {
            "Failed to send whisper - Twitch did not allow recipient lookup for the active account."
                .to_string()
        }
        WhisperRecipientLookupError::RateLimited => {
            "Failed to send whisper - Twitch is rate-limiting recipient lookup. Try again shortly."
                .to_string()
        }
        WhisperRecipientLookupError::ServiceUnavailable => {
            "Failed to send whisper - Twitch recipient lookup is temporarily unavailable."
                .to_string()
        }
        WhisperRecipientLookupError::Transport
        | WhisperRecipientLookupError::InvalidResponse
        | WhisperRecipientLookupError::UnexpectedResponse => {
            "Failed to send whisper - Could not look up the recipient right now.".to_string()
        }
    }
}

async fn lookup_whisper_recipient_id(
    target: &str,
    token: &str,
) -> Result<String, WhisperRecipientLookupError> {
    let login =
        normalize_whisper_target(target).ok_or(WhisperRecipientLookupError::InvalidLogin)?;
    let url = format!(
        "https://api.twitch.tv/helix/users?login={}",
        urlencoding::encode(&login)
    );
    let response = send_helix_idempotent(token, |token| {
        TWITCH_HTTP_CLIENT.get(url.clone()).helix_auth(token)
    })
    .await
    .map_err(|_| WhisperRecipientLookupError::Transport)?;
    let status = response.status().as_u16();
    if status != 200 {
        return Err(match status {
            401 => WhisperRecipientLookupError::Authentication,
            403 => WhisperRecipientLookupError::Permission,
            429 => WhisperRecipientLookupError::RateLimited,
            500..=599 => WhisperRecipientLookupError::ServiceUnavailable,
            _ => WhisperRecipientLookupError::UnexpectedResponse,
        });
    }
    let response = response
        .json::<HelixWhisperRecipientLookup>()
        .await
        .map_err(|_| WhisperRecipientLookupError::InvalidResponse)?;
    let Some(recipient) = response.data.into_iter().next() else {
        return Err(WhisperRecipientLookupError::NotFound);
    };
    if recipient.id.trim().is_empty() {
        return Err(WhisperRecipientLookupError::InvalidResponse);
    }
    Ok(recipient.id)
}

fn whisper_request_url(from_user_id: &str, to_user_id: &str) -> String {
    format!(
        "{HELIX_WHISPERS_ENDPOINT}?from_user_id={}&to_user_id={}",
        urlencoding::encode(from_user_id),
        urlencoding::encode(to_user_id)
    )
}

fn whisper_request_body(message: &str) -> serde_json::Value {
    serde_json::json!({ "message": message })
}

fn sanitize_whisper_error_message(raw: &str) -> String {
    let parsed = serde_json::from_str::<HelixWhisperErrorPayload>(raw)
        .ok()
        .map(|payload| payload.message)
        .filter(|message| !message.trim().is_empty())
        .unwrap_or_else(|| raw.to_string());
    let mut cleaned = String::with_capacity(parsed.len().min(WHISPER_ERROR_MAX_CHARS));
    let mut previous_was_space = false;
    let mut cleaned_chars = 0;
    for ch in parsed.chars() {
        let ch = if ch.is_control() { ' ' } else { ch };
        if ch.is_whitespace() {
            if previous_was_space {
                continue;
            }
            previous_was_space = true;
            cleaned.push(' ');
        } else {
            previous_was_space = false;
            cleaned.push(ch);
        }
        cleaned_chars += 1;
        if cleaned_chars >= WHISPER_ERROR_MAX_CHARS {
            break;
        }
    }
    cleaned.trim().to_string()
}

fn format_whisper_api_error(status: u16, raw: &str) -> String {
    let message = sanitize_whisper_error_message(raw);
    let lower = message.to_ascii_lowercase();
    let detail = match status {
        400 if lower.starts_with("a user cannot whisper themself") => {
            "You cannot whisper yourself.".to_string()
        }
        401 if lower.starts_with("missing scope") => {
            "Missing required scope. Re-login with your account and try again.".to_string()
        }
        401 if lower.starts_with("the sender does not have a verified phone number") => {
            "Due to Twitch restrictions, you are required to have a verified phone number to send whispers. Add one in Twitch Settings > Security and Privacy."
                .to_string()
        }
        403
            if lower.starts_with(
                "the recipient's settings prevent this sender from whispering them",
            ) =>
        {
            "The recipient doesn't allow whispers from strangers or from you directly."
                .to_string()
        }
        403 => "You don't have permission to perform that action.".to_string(),
        429 => "You may only whisper a maximum of 40 unique recipients per day. Within that limit, Twitch allows a maximum of 3 whispers per second and 100 whispers per minute."
            .to_string(),
        400 | 401 | 404 if !message.is_empty() => message,
        _ => "An unknown error has occurred.".to_string(),
    };
    format!("Failed to send whisper - {detail}")
}

/// Deterministic contract consumed by the opt-in UI interaction fixture.
/// The same builders and error mapper are used by the live Helix request.
pub(crate) fn whisper_contract_snapshot() -> WhisperContractSnapshot {
    let request_exact = whisper_request_url("111", "222")
        == "https://api.twitch.tv/helix/whispers?from_user_id=111&to_user_id=222"
        && whisper_request_body("hello") == serde_json::json!({ "message": "hello" });
    let mapped = [
        format_whisper_api_error(400, r#"{"message":"A user cannot whisper themself"}"#),
        format_whisper_api_error(400, r#"{"message":"Forwarded 400 detail"}"#),
        format_whisper_api_error(401, r#"{"message":"Missing scope: user:manage:whispers"}"#),
        format_whisper_api_error(
            401,
            r#"{"message":"the sender does not have a verified phone number"}"#,
        ),
        format_whisper_api_error(401, r#"{"message":"Forwarded 401 detail"}"#),
        format_whisper_api_error(
            403,
            r#"{"message":"The recipient's settings prevent this sender from whispering them"}"#,
        ),
        format_whisper_api_error(403, r#"{"message":"Forbidden"}"#),
        format_whisper_api_error(404, r#"{"message":"Forwarded 404 detail"}"#),
        format_whisper_api_error(429, r#"{"message":"Too Many Requests"}"#),
        format_whisper_api_error(500, r#"{"message":"Internal Server Error"}"#),
        format_whisper_api_error(400, ""),
    ];
    let expected_fragments = [
        "cannot whisper yourself",
        "Forwarded 400 detail",
        "Missing required scope",
        "verified phone number",
        "Forwarded 401 detail",
        "doesn't allow whispers",
        "don't have permission",
        "Forwarded 404 detail",
        "40 unique recipients",
        "unknown error",
        "unknown error",
    ];
    let sanitizer_bounded =
        sanitize_whisper_error_message(r#"{"message":"private\u0000   detail\nnext"}"#)
            == "private detail next"
            && sanitize_whisper_error_message(&"x".repeat(WHISPER_ERROR_MAX_CHARS + 64))
                .chars()
                .count()
                == WHISPER_ERROR_MAX_CHARS;
    WhisperContractSnapshot {
        endpoint: HELIX_WHISPERS_ENDPOINT,
        scope: TWITCH_WHISPER_MANAGE_SCOPE,
        scope_requested: crate::backend::auth::oauth_scope_requested(TWITCH_WHISPER_MANAGE_SCOPE),
        request_exact,
        login_validation: normalize_whisper_target("Valid_Name1").as_deref() == Some("valid_name1")
            && normalize_whisper_target("bad&login").is_none()
            && normalize_whisper_target("").is_none()
            && normalize_whisper_target("abcdefghijklmnopqrstuvwxyz").is_none(),
        api_error_cases: mapped.len(),
        errors_mapped: mapped
            .iter()
            .zip(expected_fragments)
            .all(|(message, fragment)| message.contains(fragment)),
        sanitizer_bounded,
    }
}

pub fn whisper_compose_block_reason() -> Option<String> {
    if let Err(error) = ensure_helix_command_token() {
        return Some(error);
    }
    crate::backend::auth::capture_authenticated_user_resource()
        .is_none()
        .then(|| {
            "Whisper sending is waiting for the active Twitch account identity to finish loading."
                .to_string()
        })
}

/// Send a Twitch whisper through Helix: resolve the recipient through Helix,
/// POST `/helix/whispers`, and publish local history only after Twitch
/// confirms success and the initiating account is current.
pub async fn cmd_whisper(target: &str, message: &str) -> CommandResult {
    if let Err(error) = ensure_helix_command_token() {
        return CommandResult::Error(error);
    }
    let auth =
        match crate::backend::auth::capture_authenticated_user_resource() {
            Some(auth) => auth,
            None => return CommandResult::Error(
                "Cannot send whispers until the active Twitch account identity finishes loading."
                    .to_string(),
            ),
        };
    if !auth.is_current() {
        return CommandResult::Error(
            "The active Twitch account changed before the whisper could be sent. Try again."
                .to_string(),
        );
    }
    let to_user_id = match lookup_whisper_recipient_id(target, &auth.access_token).await {
        Ok(user_id) => user_id,
        Err(error) => {
            return CommandResult::Error(format_whisper_recipient_lookup_error(error));
        }
    };
    if !auth.is_current() {
        return CommandResult::Error(
            "The active Twitch account changed before the whisper could be sent. Try again."
                .to_string(),
        );
    }
    let url = whisper_request_url(&auth.user_id, &to_user_id);
    match TWITCH_HTTP_CLIENT
        .post(&url)
        .helix_auth(&auth.access_token)
        .json(&whisper_request_body(message))
        .send()
        .await
    {
        Ok(response) if response.status().is_success() => {
            if response.status().as_u16() != 204 {
                crate::log_net!(
                    warn,
                    "Twitch whisper returned successful status {}, expected 204",
                    response.status()
                );
            }
            let _ = auth.commit_if_current(|| {
                crate::backend::whispers::record_sent(
                    target,
                    &to_user_id,
                    message,
                    chrono::Utc::now().timestamp_millis(),
                );
            });
            CommandResult::Success(None)
        }
        Ok(response) => {
            let status = response.status();
            let body = crate::backend::http::error_body(response).await;
            if let Some(error) =
                renewed_session_error(status, &auth.access_token, &body, "Failed to send whisper")
                    .await
            {
                return error;
            }
            CommandResult::http_error(
                status.as_u16(),
                format_whisper_api_error(status.as_u16(), &body),
            )
        }
        Err(error) => {
            crate::log_net!(
                warn,
                "Twitch whisper request failed before Twitch returned a response: {}",
                crate::backend::http::error_kind(&error)
            );
            CommandResult::DeliveryUnknown(
                "Failed to send whisper - An unknown error has occurred.".to_string(),
            )
        }
    }
}

const MAX_CHATTERS_PAGES: usize = 500;
const ROLE_LIST_PAGE_SIZE: usize = 100;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AutomodMessageAction {
    Allow,
    Deny,
}

impl AutomodMessageAction {
    fn api_value(self) -> &'static str {
        match self {
            Self::Allow => "ALLOW",
            Self::Deny => "DENY",
        }
    }

    fn verb(self) -> &'static str {
        match self {
            Self::Allow => "allow",
            Self::Deny => "deny",
        }
    }

    fn past_tense(self) -> &'static str {
        match self {
            Self::Allow => "Allowed",
            Self::Deny => "Denied",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SuspiciousUserAction {
    Monitor,
    Restrict,
    Clear,
}

impl SuspiciousUserAction {
    fn api_status(self) -> Option<&'static str> {
        match self {
            Self::Monitor => Some("ACTIVE_MONITORING"),
            Self::Restrict => Some("RESTRICTED"),
            Self::Clear => None,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Monitor => "monitor",
            Self::Restrict => "restrict",
            Self::Clear => "clear suspicious-user status for",
        }
    }

    fn success_message(self, username: &str) -> String {
        match self {
            Self::Monitor => format!("Monitoring suspicious user {username}"),
            Self::Restrict => format!("Restricted suspicious user {username}"),
            Self::Clear => format!("Cleared suspicious-user status for {username}"),
        }
    }
}

async fn get_channel_id_for_command(channel: &str) -> Result<String, String> {
    if let Some(id) = get_cached_channel_id(channel) {
        return Ok(id);
    }
    get_user_id_cached(channel)
        .await
        .map_err(|e| format!("Could not resolve channel ID for #{channel}: {e}"))
}

#[derive(Clone, Copy)]
enum ChannelRoleAction {
    AddModerator,
    RemoveModerator,
    AddVip,
    RemoveVip,
}

impl ChannelRoleAction {
    fn usage(self) -> &'static str {
        match self {
            Self::AddModerator => "Usage: /mod <username>",
            Self::RemoveModerator => "Usage: /unmod <username>",
            Self::AddVip => "Usage: /vip <username>",
            Self::RemoveVip => "Usage: /unvip <username>",
        }
    }

    fn path(self) -> &'static str {
        match self {
            Self::AddModerator | Self::RemoveModerator => "moderation/moderators",
            Self::AddVip | Self::RemoveVip => "channels/vips",
        }
    }

    fn is_delete(self) -> bool {
        matches!(self, Self::RemoveModerator | Self::RemoveVip)
    }

    fn success_message(self, username: &str) -> String {
        match self {
            Self::AddModerator => {
                format!("You have added {username} as a moderator of this channel.")
            }
            Self::RemoveModerator => {
                format!("You have removed {username} as a moderator of this channel.")
            }
            Self::AddVip => format!("You have added {username} as a VIP of this channel."),
            Self::RemoveVip => format!("You have removed {username} as a VIP of this channel."),
        }
    }

    fn failure_prefix(self) -> &'static str {
        match self {
            Self::AddModerator => "Failed to add channel moderator",
            Self::RemoveModerator => "Failed to remove channel moderator",
            Self::AddVip => "Failed to add VIP",
            Self::RemoveVip => "Failed to remove VIP",
        }
    }

    fn required_scope(self) -> &'static str {
        match self {
            Self::AddModerator | Self::RemoveModerator => "channel:manage:moderators",
            Self::AddVip | Self::RemoveVip => "channel:manage:vips",
        }
    }
}

pub async fn manage_suspicious_user_status(
    channel: &str,
    username: &str,
    user_id: Option<&str>,
    action: SuspiciousUserAction,
) -> CommandResult {
    let username = username
        .trim()
        .trim_start_matches(['@', '#'])
        .to_lowercase();
    let target_id = match user_id
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .map(str::to_string)
    {
        Some(id) => id,
        None if !username.is_empty() => match get_user_id_cached(&username).await {
            Ok(id) => id,
            Err(error) => {
                return CommandResult::Error(format!("Could not resolve user {username}: {error}"));
            }
        },
        None => return CommandResult::Error("Usage: /monitor <username>".to_string()),
    };
    let target_label = if username.is_empty() {
        target_id.clone()
    } else {
        username.clone()
    };

    let broadcaster_id = match get_channel_id_for_command(channel).await {
        Ok(id) => id,
        Err(error) => return CommandResult::Error(error),
    };
    let moderator_id = match get_current_user_id().await {
        Ok(id) => id,
        Err(error) => return CommandResult::Error(error),
    };
    let token = match get_token() {
        Some(token) => token,
        None => return CommandResult::Error("Not authenticated".to_string()),
    };

    let base_url = format!(
        "https://api.twitch.tv/helix/moderation/suspicious_users?broadcaster_id={}&moderator_id={}",
        urlencoding::encode(&broadcaster_id),
        urlencoding::encode(&moderator_id),
    );

    let response = match action.api_status() {
        Some(status) => {
            let body = serde_json::json!({
                "user_id": target_id,
                "status": status,
            });
            send_helix_idempotent(&token, |token| {
                TWITCH_HTTP_CLIENT
                    .post(&base_url)
                    .helix_auth(token)
                    .json(&body)
            })
            .await
        }
        None => {
            let url = format!("{}&user_id={}", base_url, urlencoding::encode(&target_id));
            send_helix_idempotent(&token, |token| {
                TWITCH_HTTP_CLIENT.delete(&url).helix_auth(token)
            })
            .await
        }
    };

    match response {
        Ok(response) if response.status().is_success() => {
            CommandResult::Success(Some(action.success_message(&target_label)))
        }
        Ok(response) => {
            let status = response.status();
            let text = crate::backend::http::error_body(response).await;
            CommandResult::http_error(
                status.as_u16(),
                format!(
                    "Failed to {} {target_label} ({status}): {text}",
                    action.label()
                ),
            )
        }
        Err(error) => CommandResult::DeliveryUnknown(format!(
            "Failed to {} {target_label}: {error}",
            action.label()
        )),
    }
}

pub async fn manage_held_automod_message(
    message_id: &str,
    action: AutomodMessageAction,
) -> CommandResult {
    let message_id = message_id.trim();
    if message_id.is_empty() {
        return CommandResult::Error("AutoMod message ID is missing".to_string());
    }

    let moderator_id = match get_current_user_id().await {
        Ok(id) => id,
        Err(error) => return CommandResult::Error(error),
    };
    let token = match get_token() {
        Some(token) => token,
        None => return CommandResult::Error("Not authenticated".to_string()),
    };

    let body = serde_json::json!({
        "user_id": moderator_id,
        "msg_id": message_id,
        "action": action.api_value(),
    });

    match send_helix_idempotent(&token, |token| {
        TWITCH_HTTP_CLIENT
            .post("https://api.twitch.tv/helix/moderation/automod/message")
            .helix_auth(token)
            .json(&body)
    })
    .await
    {
        Ok(response) if response.status().is_success() => {
            CommandResult::Success(Some(format!("{} AutoMod message", action.past_tense())))
        }
        Ok(response) => {
            let status = response.status();
            let text = crate::backend::http::error_body(response).await;
            CommandResult::http_error(
                status.as_u16(),
                format!(
                    "Failed to {} AutoMod message ({status}): {text}",
                    action.verb()
                ),
            )
        }
        Err(error) => CommandResult::DeliveryUnknown(format!(
            "Failed to {} AutoMod message: {error}",
            action.verb()
        )),
    }
}

async fn modify_channel_role(
    channel: &str,
    args: &str,
    action: ChannelRoleAction,
) -> CommandResult {
    let auth = match crate::backend::auth::capture_authenticated_user_resource() {
        Some(auth) => auth,
        None => {
            return CommandResult::Error(
                "Cannot change channel roles until the active Twitch account identity finishes loading."
                    .to_string(),
            );
        }
    };
    modify_channel_role_with_auth(channel, args, action, &auth).await
}

/// The publication decision for a typed remote moderation command. A completed request can
/// belong to an account that is no longer active, so callers must not turn a
/// stale result into either a success toast or an ordinary failure toast.
pub(crate) enum FencedModerationCommandPublication {
    Publish(CommandResult),
    Suppress {
        remote_request_reported_success: bool,
    },
}

/// A moderation-command result paired with the account resource that authorized it.
/// Unlike a generic `CommandResult`, this keeps the account-generation fence
/// alive until the chat bridge decides whether local feedback is still safe.
pub(crate) struct FencedModerationCommandResult {
    result: CommandResult,
    auth: Option<crate::backend::auth::AuthenticatedUserResource>,
}

impl FencedModerationCommandResult {
    pub(crate) fn publication(self) -> FencedModerationCommandPublication {
        let remote_request_reported_success = matches!(self.result, CommandResult::Success(_));
        match self.auth {
            Some(auth) => auth.commit_if_current(|| self.result).map_or(
                FencedModerationCommandPublication::Suppress {
                    remote_request_reported_success,
                },
                FencedModerationCommandPublication::Publish,
            ),
            None => FencedModerationCommandPublication::Publish(self.result),
        }
    }
}

/// Execute every remote moderation command with one captured
/// account resource from the chat submission boundary. `None` leaves all
/// non-moderation command dispatch unchanged; for a moderation command it
/// remains a publishable local preflight error rather than adopting an account
/// that logged in while custom-command expansion was awaiting data.
pub(crate) async fn execute_typed_moderation_command(
    channel: &str,
    input: &str,
    auth: Option<crate::backend::auth::AuthenticatedUserResource>,
) -> Option<FencedModerationCommandResult> {
    let input = input.trim();
    if !input.starts_with('/') || !crate::backend::settings::get_slash_commands_enabled() {
        return None;
    }

    let mut parts = input[1..].splitn(2, char::is_whitespace);
    let command = parts.next()?.to_ascii_lowercase();
    let args = parts.next().unwrap_or("").trim();
    let is_remote_moderation = matches!(
        command.as_str(),
        "timeout"
            | "to"
            | "ban"
            | "banid"
            | "unban"
            | "untimeout"
            | "delete"
            | "clear"
            | "warn"
            | "mod"
            | "unmod"
            | "vip"
            | "unvip"
    );
    if !is_remote_moderation {
        return None;
    }

    let result = match auth.as_ref() {
        Some(auth) => match command.as_str() {
            "timeout" | "to" => super::twitch::cmd_timeout_with_auth(channel, args, auth).await,
            "ban" => super::twitch::cmd_ban_with_auth(channel, args, auth).await,
            "banid" => super::twitch::cmd_banid_with_auth(channel, args, auth).await,
            "unban" => super::twitch::cmd_unban_with_auth(channel, args, auth).await,
            "untimeout" => super::twitch::cmd_untimeout_with_auth(channel, args, auth).await,
            "delete" => super::twitch::cmd_delete_with_auth(channel, args, auth).await,
            "clear" => delete_chat_messages_with_auth(channel, None, auth).await,
            "warn" => cmd_warn_with_auth(channel, args, auth).await,
            "mod" => modify_channel_role_with_auth(channel, args, ChannelRoleAction::AddModerator, auth).await,
            "unmod" => modify_channel_role_with_auth(channel, args, ChannelRoleAction::RemoveModerator, auth).await,
            "vip" => modify_channel_role_with_auth(channel, args, ChannelRoleAction::AddVip, auth).await,
            "unvip" => modify_channel_role_with_auth(channel, args, ChannelRoleAction::RemoveVip, auth).await,
            _ => unreachable!("remote moderation command already matched"),
        },
        None => CommandResult::Error(
            "Cannot run moderation commands until the active Twitch account identity finishes loading."
                .to_string(),
        ),
    };
    Some(FencedModerationCommandResult { result, auth })
}

/// Execute one broadcaster-owned role mutation with a token and user ID captured
/// from the same account generation. Callers that publish local UI state retain
/// the resource so they can fence that publication after this request completes.
async fn modify_channel_role_with_auth(
    channel: &str,
    args: &str,
    action: ChannelRoleAction,
    auth: &crate::backend::auth::AuthenticatedUserResource,
) -> CommandResult {
    let username = match args.split_whitespace().next() {
        Some(username) => username.trim_start_matches(['@', '#']).to_lowercase(),
        None => return CommandResult::Error(action.usage().to_string()),
    };
    if username.is_empty() {
        return CommandResult::Error(action.usage().to_string());
    }

    if !auth.is_current() {
        return CommandResult::Error(
            "The active Twitch account changed before the role could be updated. Try again."
                .to_string(),
        );
    }

    let broadcaster_id = match get_user_id_for_role(channel, &auth.access_token).await {
        Ok(id) => id,
        Err(e) => return CommandResult::Error(e),
    };
    let target_id = match get_user_id_for_role(&username, &auth.access_token).await {
        Ok(id) => id,
        Err(e) => return CommandResult::Error(format!("Invalid username {username}: {e}")),
    };

    if broadcaster_id != auth.user_id {
        return CommandResult::Error(
            "Only the broadcaster may change channel moderator or VIP roles.".to_string(),
        );
    }

    let url = format!(
        "https://api.twitch.tv/helix/{}?broadcaster_id={}&user_id={}",
        action.path(),
        broadcaster_id,
        target_id
    );
    if !auth.is_current() {
        return CommandResult::Error(
            "The active Twitch account changed before the role could be updated. Try again."
                .to_string(),
        );
    }
    let response = send_helix_idempotent(&auth.access_token, |token| {
        if action.is_delete() {
            TWITCH_HTTP_CLIENT.delete(&url)
        } else {
            TWITCH_HTTP_CLIENT.post(&url)
        }
        .helix_auth(token)
    })
    .await;
    match response {
        Ok(resp) if resp.status().is_success() => {
            CommandResult::Success(Some(action.success_message(&username)))
        }
        Ok(resp) => {
            let status = resp.status();
            let text = crate::backend::http::error_body(resp).await;
            if status == reqwest::StatusCode::UNAUTHORIZED
                && text.to_ascii_lowercase().contains("scope")
            {
                return CommandResult::Error(format!(
                    "{}: the active Twitch account lacks the `{}` permission. Sign out and sign back in to Twitch in Echo, approve the requested permissions, then try again. Reconnecting chat alone will not add this permission.",
                    action.failure_prefix(),
                    action.required_scope(),
                ));
            }
            CommandResult::http_error(
                status.as_u16(),
                format!("{} ({}): {}", action.failure_prefix(), status, text),
            )
        }
        Err(e) => CommandResult::DeliveryUnknown(format!(
            "{} request failed: {e}",
            action.failure_prefix()
        )),
    }
}

/// Resolve a login using only the supplied account-generation token on cache
/// misses. This avoids falling back to whichever account is current after an
/// asynchronous role action has already captured its credentials.
async fn get_user_id_for_role(login: &str, token: &str) -> Result<String, String> {
    let login = login.trim().trim_start_matches(['@', '#']);
    if login.is_empty() {
        return Err("A Twitch login is required".to_string());
    }

    if let Some(id) = get_cached_channel_id(login) {
        return Ok(id);
    }

    let id = crate::backend::twitch_api::get_user_id_helix(login, token)
        .await
        .map_err(|error| format!("Could not resolve Twitch ID for {login}: {error}"))?;
    crate::backend::twitch_api::cache_channel_id(login, &id);
    Ok(id)
}

struct ChattersFetch {
    chatters: Vec<HelixChatter>,
    total: Option<usize>,
}

#[derive(Deserialize)]
struct RoleListPage {
    data: Vec<RoleListUser>,
    pagination: Option<RoleListPagination>,
}

#[derive(Deserialize)]
struct RoleListUser {
    user_id: String,
    user_login: String,
    user_name: String,
}

#[derive(Deserialize)]
struct RoleListPagination {
    cursor: Option<String>,
}

#[derive(Clone, Copy)]
enum RoleListKind {
    Moderators,
    Vips,
}

impl RoleListKind {
    fn endpoint(self) -> &'static str {
        match self {
            Self::Moderators => "https://api.twitch.tv/helix/moderation/moderators",
            Self::Vips => "https://api.twitch.tv/helix/channels/vips",
        }
    }

    fn empty_message(self) -> &'static str {
        match self {
            Self::Moderators => "This channel does not have any moderators.",
            Self::Vips => "This channel does not have any VIPs.",
        }
    }

    fn failure_prefix(self) -> &'static str {
        match self {
            Self::Moderators => "Failed to get moderators",
            Self::Vips => "Failed to list VIPs",
        }
    }

    fn loaded_label(self) -> &'static str {
        match self {
            Self::Moderators => "moderators",
            Self::Vips => "VIPs",
        }
    }

    fn source(self) -> &'static str {
        match self {
            Self::Moderators => "Twitch moderators",
            Self::Vips => "Twitch VIPs",
        }
    }
}

fn role_list_url(kind: RoleListKind, broadcaster_id: &str, after: Option<&str>) -> Url {
    let mut url = Url::parse(kind.endpoint()).expect("static Twitch role-list URL must be valid");
    let first = ROLE_LIST_PAGE_SIZE.to_string();
    {
        let mut query = url.query_pairs_mut();
        query
            .append_pair("broadcaster_id", broadcaster_id)
            .append_pair("first", &first);
        if let Some(after) = after {
            query.append_pair("after", after);
        }
    }
    url
}

#[cfg(test)]
mod role_list_url_tests {
    use super::*;
    use core::prelude::v1::test;

    #[test]
    fn builds_role_endpoints_and_omits_absent_cursor() {
        assert_eq!(
            role_list_url(RoleListKind::Moderators, "123456", None).as_str(),
            "https://api.twitch.tv/helix/moderation/moderators?broadcaster_id=123456&first=100"
        );
        assert_eq!(
            role_list_url(RoleListKind::Vips, "123456", None).as_str(),
            "https://api.twitch.tv/helix/channels/vips?broadcaster_id=123456&first=100"
        );
    }

    #[test]
    fn preserves_role_query_order_and_opaque_cursor_value() {
        let url = role_list_url(RoleListKind::Moderators, "123456", Some("cursor+/="));
        assert_eq!(
            url.as_str(),
            "https://api.twitch.tv/helix/moderation/moderators?broadcaster_id=123456&first=100&after=cursor%2B%2F%3D"
        );
        assert_eq!(
            url.query_pairs().into_owned().collect::<Vec<_>>(),
            vec![
                ("broadcaster_id".to_string(), "123456".to_string()),
                ("first".to_string(), "100".to_string()),
                ("after".to_string(), "cursor+/=".to_string()),
            ]
        );
    }
}

async fn fetch_role_list(
    broadcaster_id: &str,
    token: &str,
    kind: RoleListKind,
) -> Result<Vec<RoleListUser>, String> {
    let mut users = Vec::new();
    let mut after: Option<String> = None;
    let mut seen_cursors = HashSet::new();

    for _ in 0..MAX_CHATTERS_PAGES {
        let url = role_list_url(kind, broadcaster_id, after.as_deref());

        let response = send_helix_idempotent(token, |token| {
            TWITCH_HTTP_CLIENT.get(url.clone()).helix_auth(token)
        })
        .await
        .map_err(|error| format!("{} request failed: {error}", kind.failure_prefix()))?;

        let status = response.status();
        let text = response
            .text()
            .await
            .map_err(|error| format!("Failed to read {} response: {error}", kind.loaded_label()))?;
        if !status.is_success() {
            return Err(format!("{} ({status}): {text}", kind.failure_prefix()));
        }

        let page: RoleListPage = serde_json::from_str(&text).map_err(|error| {
            format!("Failed to parse {} response: {error}", kind.loaded_label())
        })?;
        users.extend(page.data);

        let next_cursor = page
            .pagination
            .and_then(|pagination| pagination.cursor)
            .filter(|cursor| !cursor.is_empty());
        match next_cursor {
            Some(cursor) if seen_cursors.insert(cursor.clone()) => after = Some(cursor),
            Some(_) | None => return Ok(users),
        }
    }

    Err(format!(
        "{} pagination exceeded {MAX_CHATTERS_PAGES} pages",
        kind.failure_prefix()
    ))
}

async fn cmd_role_list(channel: &str, kind: RoleListKind) -> CommandResult {
    let channel = channel.trim_start_matches('#').to_lowercase();
    let token = match get_token() {
        Some(token) => token,
        None => return CommandResult::Error("Not authenticated".to_string()),
    };
    let broadcaster_id = match get_channel_id_for_command(&channel).await {
        Ok(id) => id,
        Err(error) => return CommandResult::Error(error),
    };
    let users = match fetch_role_list(&broadcaster_id, &token, kind).await {
        Ok(users) => users,
        Err(error) => return CommandResult::Error(error),
    };
    if users.is_empty() {
        return CommandResult::Success(Some(kind.empty_message().to_string()));
    }

    let chatters = normalize_chatters(
        users
            .into_iter()
            .map(|user| HelixChatter {
                user_id: user.user_id,
                user_login: user.user_login,
                user_name: user.user_name,
            })
            .collect(),
    );
    let entries = chatter_list_entries(&chatters);
    let count = entries.len();

    crate::backend::ui_actions::push_action(
        crate::backend::ui_actions::UiAction::ShowChatterList {
            channel: channel.clone(),
            chatters: entries,
            total_count: count,
            source: kind.source().to_string(),
            login_sorted: true,
            is_loading: false,
        },
    );

    CommandResult::Success(Some(format!(
        "Loaded {count} {} for #{channel}",
        kind.loaded_label()
    )))
}

const POLL_USAGE: &str = "Usage: /poll --title|-t <title> --duration|-d <duration>[s|m] --choice|-c <choice1> --choice|-c <choice2> [--points|-p <points>]";
const MIN_POLL_DURATION_SECONDS: u64 = 10;
const MAX_POLL_DURATION_SECONDS: u64 = 1800;
const PREDICTION_USAGE: &str = "Usage: /prediction --title|-t <title> --duration|-d <duration>[s|m] --choice|-c <choice1> --choice|-c <choice2> [--points|-p <points>]";
const COMPLETE_PREDICTION_USAGE: &str =
    "Usage: /completeprediction --choice|-c <choice> or /completeprediction --index|-i <index>";
const MIN_PREDICTION_DURATION_SECONDS: u64 = 30;
const MAX_PREDICTION_DURATION_SECONDS: u64 = 1800;

struct PollCreateArgs {
    title: String,
    choices: Vec<String>,
    duration_seconds: u64,
    points_per_vote: Option<u64>,
}

#[derive(Deserialize)]
struct PollsResponse {
    data: Vec<PollData>,
}

#[derive(Deserialize)]
struct PollData {
    id: String,
    title: String,
    status: String,
    #[serde(default)]
    choices: Vec<PollChoiceData>,
}

#[derive(Clone, Deserialize)]
struct PollChoiceData {
    title: String,
    #[serde(default)]
    votes: u64,
}

struct PredictionCreateArgs {
    title: String,
    outcomes: Vec<String>,
    prediction_window_seconds: u64,
}

enum PredictionResolveTarget {
    Choice(String),
    Index(usize),
}

#[derive(Deserialize)]
struct PredictionsResponse {
    data: Vec<PredictionData>,
}

#[derive(Deserialize)]
struct PredictionData {
    id: String,
    title: String,
    status: String,
    #[serde(default)]
    outcomes: Vec<PredictionOutcomeData>,
}

#[derive(Clone, Deserialize)]
struct PredictionOutcomeData {
    id: String,
    title: String,
    #[serde(default)]
    users: u64,
    #[serde(default)]
    channel_points: u64,
}

fn parse_duration_to_seconds(value: &str) -> Option<u64> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }

    let digit_count = value.chars().take_while(|ch| ch.is_ascii_digit()).count();
    if digit_count == 0 {
        return None;
    }

    let amount = value[..digit_count].parse::<u64>().ok()?;
    let unit = value[digit_count..].trim().to_ascii_lowercase();
    match unit.as_str() {
        "" | "s" | "sec" | "secs" | "second" | "seconds" => Some(amount),
        "m" | "min" | "mins" | "minute" | "minutes" => Some(amount.saturating_mul(60)),
        _ => None,
    }
}

fn take_option_value(
    args: &[String],
    index: &mut usize,
    option: &str,
    usage: &str,
) -> Result<String, String> {
    *index += 1;
    args.get(*index)
        .filter(|value| !value.starts_with('-'))
        .cloned()
        .ok_or_else(|| format!("Missing value for {option} - {usage}"))
}

fn parse_poll_create_args(input: &str) -> Result<PollCreateArgs, String> {
    let args = super::local_core::split_command_args(input)
        .map_err(|error| format!("Invalid poll arguments: {error}"))?;
    let mut title = None;
    let mut duration_seconds = None;
    let mut choices = Vec::new();
    let mut points_per_vote = None;

    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--title" | "-t" => {
                title = Some(take_option_value(&args, &mut index, "--title", POLL_USAGE)?)
            }
            "--duration" | "-d" => {
                let raw = take_option_value(&args, &mut index, "--duration", POLL_USAGE)?;
                let parsed = parse_duration_to_seconds(&raw)
                    .ok_or_else(|| format!("Invalid duration - {POLL_USAGE}"))?;
                duration_seconds =
                    Some(parsed.clamp(MIN_POLL_DURATION_SECONDS, MAX_POLL_DURATION_SECONDS));
            }
            "--choice" | "-c" => {
                choices.push(take_option_value(
                    &args, &mut index, "--choice", POLL_USAGE,
                )?);
            }
            "--points" | "-p" => {
                let raw = take_option_value(&args, &mut index, "--points", POLL_USAGE)?;
                points_per_vote = Some(
                    raw.parse::<u64>()
                        .map_err(|_| format!("Invalid points - {POLL_USAGE}"))?,
                );
            }
            other => return Err(format!("Unknown poll option {other} - {POLL_USAGE}")),
        }
        index += 1;
    }

    let title = title.ok_or_else(|| format!("Missing title - {POLL_USAGE}"))?;
    if title.chars().count() > 60 {
        return Err(format!(
            "Poll title may not exceed 60 characters - {POLL_USAGE}"
        ));
    }
    if choices.len() < 2 || choices.len() > 5 {
        return Err(format!(
            "Poll requires between two and five choices - {POLL_USAGE}"
        ));
    }
    if choices.iter().any(|choice| choice.trim().is_empty()) {
        return Err(format!("Poll choices may not be empty - {POLL_USAGE}"));
    }
    let duration_seconds =
        duration_seconds.ok_or_else(|| format!("Missing duration - {POLL_USAGE}"))?;

    Ok(PollCreateArgs {
        title,
        choices,
        duration_seconds,
        points_per_vote: points_per_vote.filter(|points| *points > 0),
    })
}

fn parse_prediction_create_args(input: &str) -> Result<PredictionCreateArgs, String> {
    let args = super::local_core::split_command_args(input)
        .map_err(|error| format!("Invalid prediction arguments: {error}"))?;
    let mut title = None;
    let mut prediction_window_seconds = None;
    let mut outcomes = Vec::new();

    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--title" | "-t" => {
                title = Some(take_option_value(
                    &args,
                    &mut index,
                    "--title",
                    PREDICTION_USAGE,
                )?)
            }
            "--duration" | "-d" => {
                let raw = take_option_value(&args, &mut index, "--duration", PREDICTION_USAGE)?;
                let parsed = parse_duration_to_seconds(&raw)
                    .ok_or_else(|| format!("Invalid duration - {PREDICTION_USAGE}"))?;
                prediction_window_seconds = Some(parsed.clamp(
                    MIN_PREDICTION_DURATION_SECONDS,
                    MAX_PREDICTION_DURATION_SECONDS,
                ));
            }
            "--choice" | "-c" => outcomes.push(take_option_value(
                &args,
                &mut index,
                "--choice",
                PREDICTION_USAGE,
            )?),
            "--points" | "-p" => {
                let raw = take_option_value(&args, &mut index, "--points", PREDICTION_USAGE)?;
                raw.parse::<u64>()
                    .map_err(|_| format!("Invalid points - {PREDICTION_USAGE}"))?;
            }
            other => {
                return Err(format!(
                    "Unknown prediction option {other} - {PREDICTION_USAGE}"
                ));
            }
        }
        index += 1;
    }

    let title = title.ok_or_else(|| format!("Missing title - {PREDICTION_USAGE}"))?;
    if title.chars().count() > 45 {
        return Err(format!(
            "Prediction title may not exceed 45 characters - {PREDICTION_USAGE}"
        ));
    }
    if outcomes.len() < 2 || outcomes.len() > 10 {
        return Err(format!(
            "Prediction requires between two and ten choices - {PREDICTION_USAGE}"
        ));
    }
    if outcomes.iter().any(|outcome| outcome.trim().is_empty()) {
        return Err(format!(
            "Prediction choices may not be empty - {PREDICTION_USAGE}"
        ));
    }
    let prediction_window_seconds = prediction_window_seconds
        .ok_or_else(|| format!("Missing duration - {PREDICTION_USAGE}"))?;

    Ok(PredictionCreateArgs {
        title,
        outcomes,
        prediction_window_seconds,
    })
}

fn parse_complete_prediction_args(input: &str) -> Result<PredictionResolveTarget, String> {
    let args = super::local_core::split_command_args(input)
        .map_err(|error| format!("Invalid completeprediction arguments: {error}"))?;
    let mut choice = None;
    let mut index = None;

    let mut arg_index = 0;
    while arg_index < args.len() {
        match args[arg_index].as_str() {
            "--choice" | "-c" => {
                choice = Some(take_option_value(
                    &args,
                    &mut arg_index,
                    "--choice",
                    COMPLETE_PREDICTION_USAGE,
                )?)
            }
            "--index" | "-i" => {
                let raw =
                    take_option_value(&args, &mut arg_index, "--index", COMPLETE_PREDICTION_USAGE)?;
                let parsed = raw
                    .parse::<usize>()
                    .ok()
                    .filter(|index| *index > 0)
                    .ok_or_else(|| format!("Invalid index - {COMPLETE_PREDICTION_USAGE}"))?;
                index = Some(parsed);
            }
            other => {
                return Err(format!(
                    "Unknown completeprediction option {other} - {COMPLETE_PREDICTION_USAGE}"
                ));
            }
        }
        arg_index += 1;
    }

    match (choice, index) {
        (Some(_), Some(_)) => Err(format!(
            "You may not specify choice and index simultaneously - {COMPLETE_PREDICTION_USAGE}"
        )),
        (None, None) => Err(format!(
            "You must specify either choice or index - {COMPLETE_PREDICTION_USAGE}"
        )),
        (Some(choice), None) => Ok(PredictionResolveTarget::Choice(choice)),
        (None, Some(index)) => Ok(PredictionResolveTarget::Index(index)),
    }
}

/// /poll --title <title> --duration <duration> --choice <choice> --choice <choice>
pub async fn cmd_poll(channel: &str, args: &str) -> CommandResult {
    let poll = match parse_poll_create_args(args) {
        Ok(poll) => poll,
        Err(error) => return CommandResult::Error(error),
    };
    let broadcaster_id = match get_channel_id_for_command(channel).await {
        Ok(id) => id,
        Err(error) => return CommandResult::Error(error),
    };
    let token = match get_token() {
        Some(token) => token,
        None => return CommandResult::Error("Not authenticated".to_string()),
    };

    let choices: Vec<_> = poll
        .choices
        .iter()
        .map(|choice| serde_json::json!({ "title": choice }))
        .collect();
    let mut body = serde_json::json!({
        "broadcaster_id": broadcaster_id,
        "title": poll.title.clone(),
        "duration": poll.duration_seconds,
        "choices": choices,
    });
    if let Some(points) = poll.points_per_vote {
        body["channel_points_voting_enabled"] = serde_json::json!(true);
        body["channel_points_per_vote"] = serde_json::json!(points);
    }

    match TWITCH_HTTP_CLIENT
        .post("https://api.twitch.tv/helix/polls")
        .helix_auth(&token)
        .json(&body)
        .send()
        .await
    {
        Ok(resp) if resp.status().is_success() => {
            CommandResult::Success(Some(format!("Created poll: '{}'", poll.title)))
        }
        Ok(resp) => {
            let status = resp.status();
            let text = crate::backend::http::error_body(resp).await;
            if let Some(error) =
                renewed_session_error(status, &token, &text, "Failed to create poll").await
            {
                return error;
            }
            CommandResult::http_error(
                status.as_u16(),
                format!("Failed to create poll ({status}): {text}"),
            )
        }
        Err(error) => CommandResult::DeliveryUnknown(format!("Poll request failed: {error}")),
    }
}

/// /prediction --title <title> --duration <duration> --choice <choice> --choice <choice>
pub async fn cmd_prediction(channel: &str, args: &str) -> CommandResult {
    let prediction = match parse_prediction_create_args(args) {
        Ok(prediction) => prediction,
        Err(error) => return CommandResult::Error(error),
    };
    let broadcaster_id = match get_channel_id_for_command(channel).await {
        Ok(id) => id,
        Err(error) => return CommandResult::Error(error),
    };
    let token = match get_token() {
        Some(token) => token,
        None => {
            return CommandResult::Error(
                "You must be logged in to create a prediction!".to_string(),
            );
        }
    };

    let outcomes: Vec<_> = prediction
        .outcomes
        .iter()
        .map(|outcome| serde_json::json!({ "title": outcome }))
        .collect();
    let body = serde_json::json!({
        "broadcaster_id": broadcaster_id,
        "title": prediction.title.clone(),
        "prediction_window": prediction.prediction_window_seconds,
        "outcomes": outcomes,
    });

    match TWITCH_HTTP_CLIENT
        .post("https://api.twitch.tv/helix/predictions")
        .helix_auth(&token)
        .json(&body)
        .send()
        .await
    {
        Ok(resp) if resp.status().is_success() => {
            CommandResult::Success(Some(format!("Created prediction: '{}'", prediction.title)))
        }
        Ok(resp) => {
            let status = resp.status();
            let text = crate::backend::http::error_body(resp).await;
            if let Some(error) =
                renewed_session_error(status, &token, &text, "Failed to create prediction").await
            {
                return error;
            }
            CommandResult::http_error(
                status.as_u16(),
                format!("Failed to create prediction ({status}): {text}"),
            )
        }
        Err(error) => CommandResult::DeliveryUnknown(format!("Prediction request failed: {error}")),
    }
}

#[derive(Clone, Copy)]
enum PredictionCloseAction {
    Lock,
    Cancel,
}

impl PredictionCloseAction {
    fn status(self) -> &'static str {
        match self {
            Self::Lock => "LOCKED",
            Self::Cancel => "CANCELED",
        }
    }

    fn auth_error(self) -> &'static str {
        match self {
            Self::Lock => "You must be logged in to lock a prediction!",
            Self::Cancel => "You must be logged in to cancel a prediction!",
        }
    }

    fn failure_prefix(self) -> &'static str {
        match self {
            Self::Lock => "Failed to lock prediction",
            Self::Cancel => "Failed to cancel prediction",
        }
    }
}

fn prediction_totals(prediction: &PredictionData) -> (u64, u64) {
    prediction
        .outcomes
        .iter()
        .fold((0, 0), |(points, users), outcome| {
            (points + outcome.channel_points, users + outcome.users)
        })
}

async fn fetch_latest_prediction(
    broadcaster_id: &str,
    token: &str,
) -> Result<PredictionData, String> {
    let url = format!(
        "https://api.twitch.tv/helix/predictions?broadcaster_id={}&first=1",
        urlencoding::encode(broadcaster_id)
    );
    let response = send_helix_idempotent(token, |token| {
        TWITCH_HTTP_CLIENT.get(&url).helix_auth(token)
    })
    .await
    .map_err(|error| format!("Failed to query predictions - {error}"))?;

    let status = response.status();
    let text = response
        .text()
        .await
        .map_err(|error| format!("Failed to read predictions response: {error}"))?;
    if !status.is_success() {
        return Err(format!("Failed to query predictions ({status}): {text}"));
    }

    let predictions: PredictionsResponse = serde_json::from_str(&text)
        .map_err(|error| format!("Failed to parse predictions response: {error}"))?;
    predictions
        .data
        .into_iter()
        .next()
        .ok_or_else(|| "Failed to find any predictions".to_string())
}

async fn update_prediction_status(
    broadcaster_id: &str,
    prediction_id: &str,
    token: &str,
    action: PredictionCloseAction,
) -> Result<PredictionData, CommandFailure> {
    let url = format!(
        "https://api.twitch.tv/helix/predictions?broadcaster_id={}&id={}&status={}",
        urlencoding::encode(broadcaster_id),
        urlencoding::encode(prediction_id),
        action.status()
    );
    let response = send_helix_idempotent(token, |token| {
        TWITCH_HTTP_CLIENT.patch(&url).helix_auth(token)
    })
    .await
    .map_err(|error| CommandFailure::unknown(format!("{} - {error}", action.failure_prefix())))?;

    let status = response.status();
    let text = response.text().await.map_err(|error| {
        let message = format!("Failed to read prediction update response: {error}");
        if status.is_success() {
            CommandFailure::unknown(message)
        } else {
            CommandFailure::http_error(status.as_u16(), message)
        }
    })?;
    if !status.is_success() {
        return Err(CommandFailure::http_error(
            status.as_u16(),
            format!("{} ({status}): {text}", action.failure_prefix()),
        ));
    }

    let predictions: PredictionsResponse = serde_json::from_str(&text).map_err(|error| {
        CommandFailure::unknown(format!(
            "Failed to parse prediction update response: {error}"
        ))
    })?;
    predictions.data.into_iter().next().ok_or_else(|| {
        CommandFailure::unknown(format!(
            "{}: Twitch returned no prediction data",
            action.failure_prefix()
        ))
    })
}

async fn resolve_prediction_status(
    broadcaster_id: &str,
    prediction_id: &str,
    winning_outcome_id: &str,
    token: &str,
) -> Result<PredictionData, CommandFailure> {
    let url = format!(
        "https://api.twitch.tv/helix/predictions?broadcaster_id={}&id={}&status=RESOLVED&winning_outcome_id={}",
        urlencoding::encode(broadcaster_id),
        urlencoding::encode(prediction_id),
        urlencoding::encode(winning_outcome_id)
    );
    let response = send_helix_idempotent(token, |token| {
        TWITCH_HTTP_CLIENT.patch(&url).helix_auth(token)
    })
    .await
    .map_err(|error| CommandFailure::unknown(format!("Failed to complete prediction - {error}")))?;

    let status = response.status();
    let text = response.text().await.map_err(|error| {
        let message = format!("Failed to read prediction completion response: {error}");
        if status.is_success() {
            CommandFailure::unknown(message)
        } else {
            CommandFailure::http_error(status.as_u16(), message)
        }
    })?;
    if !status.is_success() {
        return Err(CommandFailure::http_error(
            status.as_u16(),
            format!("Failed to complete prediction ({status}): {text}"),
        ));
    }

    let predictions: PredictionsResponse = serde_json::from_str(&text).map_err(|error| {
        CommandFailure::unknown(format!(
            "Failed to parse prediction completion response: {error}"
        ))
    })?;
    predictions.data.into_iter().next().ok_or_else(|| {
        CommandFailure::unknown(
            "Failed to complete prediction: Twitch returned no prediction data".to_string(),
        )
    })
}

async fn close_prediction(channel: &str, action: PredictionCloseAction) -> CommandResult {
    let broadcaster_id = match get_channel_id_for_command(channel).await {
        Ok(id) => id,
        Err(error) => return CommandResult::Error(error),
    };
    let token = match get_token() {
        Some(token) => token,
        None => return CommandResult::Error(action.auth_error().to_string()),
    };

    let prediction = match fetch_latest_prediction(&broadcaster_id, &token).await {
        Ok(prediction) => prediction,
        Err(error) => return CommandResult::Error(error),
    };

    match action {
        PredictionCloseAction::Lock if prediction.status == "LOCKED" => {
            return CommandResult::Success(Some(format!(
                "The current prediction is already locked: {}",
                prediction.title
            )));
        }
        PredictionCloseAction::Lock if prediction.status != "ACTIVE" => {
            return CommandResult::Error("Could not find an active prediction".to_string());
        }
        PredictionCloseAction::Cancel
            if prediction.status != "ACTIVE" && prediction.status != "LOCKED" =>
        {
            return CommandResult::Error("Could not find an open prediction".to_string());
        }
        _ => {}
    }

    match update_prediction_status(&broadcaster_id, &prediction.id, &token, action).await {
        Ok(prediction) => {
            let (points, users) = prediction_totals(&prediction);
            let message = match action {
                PredictionCloseAction::Lock => format!(
                    "Locked prediction with {points} points wagered by {users} users: '{}'",
                    prediction.title
                ),
                PredictionCloseAction::Cancel => format!(
                    "Refunded {points} points to {users} users for prediction: '{}'",
                    prediction.title
                ),
            };
            CommandResult::Success(Some(message))
        }
        Err(error) => CommandResult::from_failure(error),
    }
}

/// /lockprediction
pub async fn cmd_lockprediction(channel: &str) -> CommandResult {
    close_prediction(channel, PredictionCloseAction::Lock).await
}

/// /cancelprediction
pub async fn cmd_cancelprediction(channel: &str) -> CommandResult {
    close_prediction(channel, PredictionCloseAction::Cancel).await
}

/// /completeprediction --choice <choice> or /completeprediction --index <index>
pub async fn cmd_completeprediction(channel: &str, args: &str) -> CommandResult {
    let target = match parse_complete_prediction_args(args) {
        Ok(target) => target,
        Err(error) => return CommandResult::Error(error),
    };
    let broadcaster_id = match get_channel_id_for_command(channel).await {
        Ok(id) => id,
        Err(error) => return CommandResult::Error(error),
    };
    let token = match get_token() {
        Some(token) => token,
        None => {
            return CommandResult::Error(
                "You must be logged in to complete a prediction!".to_string(),
            );
        }
    };

    let prediction = match fetch_latest_prediction(&broadcaster_id, &token).await {
        Ok(prediction) => prediction,
        Err(error) if error == "Failed to find any predictions" => {
            return CommandResult::Error(
                "You must start a prediction before you can complete one".to_string(),
            );
        }
        Err(error) => return CommandResult::Error(error),
    };
    if prediction.status != "ACTIVE" && prediction.status != "LOCKED" {
        return CommandResult::Error("Could not find an open prediction to complete".to_string());
    }

    let winner_id = match target {
        PredictionResolveTarget::Index(index) => match prediction.outcomes.get(index - 1) {
            Some(outcome) => outcome.id.clone(),
            None => {
                return CommandResult::Error(format!(
                    "Specified index ({index}) exceeds the number of outcomes ({})",
                    prediction.outcomes.len()
                ));
            }
        },
        PredictionResolveTarget::Choice(choice) => match prediction
            .outcomes
            .iter()
            .find(|outcome| outcome.title == choice)
        {
            Some(outcome) => outcome.id.clone(),
            None => {
                let options = prediction
                    .outcomes
                    .iter()
                    .map(|outcome| format!("'{}'", outcome.title))
                    .collect::<Vec<_>>()
                    .join(", ");
                return CommandResult::Error(format!(
                    "Could not find the desired winner. Options include: {options}"
                ));
            }
        },
    };

    match resolve_prediction_status(&broadcaster_id, &prediction.id, &winner_id, &token).await {
        Ok(result) => {
            let total_points: u64 = result
                .outcomes
                .iter()
                .map(|outcome| outcome.channel_points)
                .sum();
            let winner = result
                .outcomes
                .iter()
                .find(|outcome| outcome.id == winner_id)
                .or_else(|| result.outcomes.first());
            match winner {
                Some(winner) => CommandResult::Success(Some(format!(
                    "Completed prediction: {} - '{}' won {} points ({} profit) to be distributed among {} users",
                    result.title,
                    winner.title,
                    total_points,
                    total_points.saturating_sub(winner.channel_points),
                    winner.users
                ))),
                None => {
                    CommandResult::Success(Some(format!("Completed prediction: {}", result.title)))
                }
            }
        }
        Err(error) => CommandResult::from_failure(error),
    }
}

#[derive(Clone, Copy)]
enum PollCloseAction {
    End,
    Cancel,
}

impl PollCloseAction {
    fn status(self) -> &'static str {
        match self {
            Self::End => "TERMINATED",
            Self::Cancel => "ARCHIVED",
        }
    }

    fn auth_error(self) -> &'static str {
        match self {
            Self::End => "You must be logged in to end a poll!",
            Self::Cancel => "You must be logged in to cancel a poll!",
        }
    }

    fn failure_prefix(self) -> &'static str {
        match self {
            Self::End => "Failed to end the poll",
            Self::Cancel => "Failed to cancel the poll",
        }
    }

    fn success_message(self, poll: &PollData) -> String {
        match self {
            Self::End => format_ended_poll_message(poll),
            Self::Cancel => format!("Canceled poll: '{}'", poll.title),
        }
    }
}

async fn fetch_latest_poll(broadcaster_id: &str, token: &str) -> Result<PollData, String> {
    let url = format!(
        "https://api.twitch.tv/helix/polls?broadcaster_id={}&first=1",
        urlencoding::encode(broadcaster_id)
    );
    let response = send_helix_idempotent(token, |token| {
        TWITCH_HTTP_CLIENT.get(&url).helix_auth(token)
    })
    .await
    .map_err(|error| format!("Failed to query polls - {error}"))?;

    let status = response.status();
    let text = response
        .text()
        .await
        .map_err(|error| format!("Failed to read polls response: {error}"))?;
    if !status.is_success() {
        return Err(format!("Failed to query polls ({status}): {text}"));
    }

    let polls: PollsResponse = serde_json::from_str(&text)
        .map_err(|error| format!("Failed to parse polls response: {error}"))?;
    polls
        .data
        .into_iter()
        .next()
        .ok_or_else(|| "Failed to find any polls".to_string())
}

async fn update_poll_status(
    broadcaster_id: &str,
    poll_id: &str,
    token: &str,
    action: PollCloseAction,
) -> Result<PollData, CommandFailure> {
    let url = format!(
        "https://api.twitch.tv/helix/polls?broadcaster_id={}&id={}&status={}",
        urlencoding::encode(broadcaster_id),
        urlencoding::encode(poll_id),
        action.status()
    );
    let response = send_helix_idempotent(token, |token| {
        TWITCH_HTTP_CLIENT.patch(&url).helix_auth(token)
    })
    .await
    .map_err(|error| CommandFailure::unknown(format!("{} - {error}", action.failure_prefix())))?;

    let status = response.status();
    let text = response.text().await.map_err(|error| {
        let message = format!("Failed to read poll update response: {error}");
        if status.is_success() {
            CommandFailure::unknown(message)
        } else {
            CommandFailure::http_error(status.as_u16(), message)
        }
    })?;
    if !status.is_success() {
        return Err(CommandFailure::http_error(
            status.as_u16(),
            format!("{} ({status}): {text}", action.failure_prefix()),
        ));
    }

    let polls: PollsResponse = serde_json::from_str(&text).map_err(|error| {
        CommandFailure::unknown(format!("Failed to parse poll update response: {error}"))
    })?;
    polls.data.into_iter().next().ok_or_else(|| {
        CommandFailure::unknown(format!(
            "{}: Twitch returned no poll data",
            action.failure_prefix()
        ))
    })
}

fn format_ended_poll_message(poll: &PollData) -> String {
    let total_votes: u64 = poll.choices.iter().map(|choice| choice.votes).sum();
    if total_votes == 0 {
        return format!("Poll ended with zero votes: '{}'", poll.title);
    }

    let winner_votes = poll
        .choices
        .iter()
        .map(|choice| choice.votes)
        .max()
        .unwrap_or(0);
    let winners: Vec<PollChoiceData> = poll
        .choices
        .iter()
        .filter(|choice| choice.votes == winner_votes)
        .cloned()
        .collect();
    if winners.len() != 1 {
        return format!("Poll ended in a draw: '{}'", poll.title);
    }

    let winner = &winners[0];
    let percent = 100.0 * winner.votes as f64 / total_votes as f64;
    format!(
        "Ended poll: '{}' - '{}' won with {} votes ({percent:.1}%)",
        poll.title, winner.title, winner.votes
    )
}

async fn close_active_poll(channel: &str, action: PollCloseAction) -> CommandResult {
    let broadcaster_id = match get_channel_id_for_command(channel).await {
        Ok(id) => id,
        Err(error) => return CommandResult::Error(error),
    };
    let token = match get_token() {
        Some(token) => token,
        None => return CommandResult::Error(action.auth_error().to_string()),
    };

    let poll = match fetch_latest_poll(&broadcaster_id, &token).await {
        Ok(poll) => poll,
        Err(error) => return CommandResult::Error(error),
    };
    if poll.status != "ACTIVE" {
        return CommandResult::Error("Could not find an active poll".to_string());
    }

    match update_poll_status(&broadcaster_id, &poll.id, &token, action).await {
        Ok(poll) => CommandResult::Success(Some(action.success_message(&poll))),
        Err(error) => CommandResult::from_failure(error),
    }
}

/// /endpoll
pub async fn cmd_endpoll(channel: &str) -> CommandResult {
    close_active_poll(channel, PollCloseAction::End).await
}

/// /cancelpoll
pub async fn cmd_cancelpoll(channel: &str) -> CommandResult {
    close_active_poll(channel, PollCloseAction::Cancel).await
}

fn chatter_list_entries(chatters: &[HelixChatter]) -> Vec<ChatterListEntry> {
    chatters
        .iter()
        .map(|chatter| ChatterListEntry {
            username: chatter.user_login.clone(),
            display_name: if chatter.user_name.is_empty() {
                chatter.user_login.clone()
            } else {
                chatter.user_name.clone()
            },
            user_id: Some(chatter.user_id.clone()),
        })
        .collect()
}

async fn fetch_all_chatters(
    channel: &str,
    broadcaster_id: &str,
    moderator_id: &str,
    token: &str,
) -> Result<ChattersFetch, String> {
    let mut chatters = Vec::new();
    let mut total = None;
    let mut after: Option<String> = None;
    let mut seen_cursors = HashSet::new();

    for _ in 0..MAX_CHATTERS_PAGES {
        let url = build_chatters_url(broadcaster_id, moderator_id, after.as_deref());
        let response = send_helix_idempotent(token, |token| {
            TWITCH_HTTP_CLIENT.get(&url).helix_auth(token)
        })
        .await
        .map_err(|error| format!("Chatters request failed: {error}"))?;

        let status = response.status();
        let text = response
            .text()
            .await
            .map_err(|error| format!("Failed to read chatters response: {error}"))?;
        if !status.is_success() {
            return Err(format!("Twitch chatters failed ({status}): {text}"));
        }

        let page = parse_chatters_page(&text)?;
        if total.is_none() {
            total = page.total;
        }
        let next_cursor = page.pagination.cursor.filter(|cursor| !cursor.is_empty());
        if chatters.is_empty() && next_cursor.is_some() {
            let first_page = normalize_chatters(page.data.clone());
            let entries = chatter_list_entries(&first_page);
            crate::backend::ui_actions::push_action(
                crate::backend::ui_actions::UiAction::ShowChatterList {
                    channel: channel.to_string(),
                    total_count: total.unwrap_or(entries.len()),
                    chatters: entries,
                    source: "Twitch".to_string(),
                    login_sorted: true,
                    is_loading: true,
                },
            );
        }
        chatters.extend(page.data);

        match next_cursor {
            Some(cursor) if seen_cursors.insert(cursor.clone()) => after = Some(cursor),
            Some(_) | None => return Ok(ChattersFetch { chatters, total }),
        }
    }

    Err(format!(
        "Twitch chatters pagination exceeded {MAX_CHATTERS_PAGES} pages"
    ))
}

/// /chatters
pub async fn cmd_chatters(channel: &str) -> CommandResult {
    let channel = channel.trim_start_matches('#').to_lowercase();

    let token = match get_token() {
        Some(token) => token,
        None => return super::local::cmd_observed_chatters(&channel, Some("not authenticated")),
    };

    let broadcaster_id = match get_channel_id_for_command(&channel).await {
        Ok(id) => id,
        Err(error) => return super::local::cmd_observed_chatters(&channel, Some(&error)),
    };

    let moderator_id = match get_current_user_id().await {
        Ok(id) => id,
        Err(error) => return super::local::cmd_observed_chatters(&channel, Some(&error)),
    };

    let result = match fetch_all_chatters(&channel, &broadcaster_id, &moderator_id, &token).await {
        Ok(result) => result,
        Err(error) => return super::local::cmd_observed_chatters(&channel, Some(&error)),
    };

    let total = result.total;
    let chatters = normalize_chatters(result.chatters);
    for chatter in chatters.iter().take(CHATTERS_PAGE_SIZE) {
        crate::backend::irc::track_chatter(&channel, &chatter.user_login, &chatter.user_name, None);
    }

    let entries = chatter_list_entries(&chatters);
    let loaded_count = entries.len();

    crate::backend::ui_actions::push_action(
        crate::backend::ui_actions::UiAction::ShowChatterList {
            channel: channel.clone(),
            chatters: entries,
            total_count: total.unwrap_or(loaded_count),
            source: "Twitch".to_string(),
            login_sorted: true,
            is_loading: false,
        },
    );

    CommandResult::Success(Some(format_chatters_status(&channel, loaded_count, total)))
}

/// /test-chatters
pub async fn cmd_test_chatters(channel: &str) -> CommandResult {
    cmd_chatters(channel).await
}

/// /mods
pub async fn cmd_mods(channel: &str) -> CommandResult {
    cmd_role_list(channel, RoleListKind::Moderators).await
}

/// /vips
pub async fn cmd_vips(channel: &str) -> CommandResult {
    cmd_role_list(channel, RoleListKind::Vips).await
}

pub async fn refresh_chatter_list_source(channel: &str, source: &str) -> CommandResult {
    if source.eq_ignore_ascii_case("Twitch moderators") {
        cmd_mods(channel).await
    } else if source.eq_ignore_ascii_case("Twitch VIPs") {
        cmd_vips(channel).await
    } else if source.eq_ignore_ascii_case("observed") {
        super::local::cmd_observed_chatters(channel, None)
    } else {
        cmd_chatters(channel).await
    }
}

/// /announce, /announceblue, /announcegreen, /announceorange, /announcepurple
pub async fn cmd_announce(channel: &str, command: &str, message: &str) -> CommandResult {
    if message.is_empty() {
        return CommandResult::Error(format!("Usage: /{command} <message>"));
    }

    let color = super::helix_core::announcement_color(command);

    let broadcaster_id = match get_cached_channel_id(channel) {
        Some(id) => id,
        None => return CommandResult::Error("Channel ID not found".to_string()),
    };

    let token = match get_token() {
        Some(t) => t,
        None => return CommandResult::Error("Not authenticated".to_string()),
    };

    let user_id = match get_current_user_id().await {
        Ok(id) => id,
        Err(e) => return CommandResult::Error(e),
    };

    let url = super::helix_core::announcement_url(&broadcaster_id, &user_id);

    let body = serde_json::json!({
        "message": message,
        "color": color
    });

    match TWITCH_HTTP_CLIENT
        .post(&url)
        .helix_auth(&token)
        .json(&body)
        .send()
        .await
    {
        Ok(resp) if resp.status().is_success() => {
            CommandResult::Success(Some(format!("Announcement sent ({})", color)))
        }
        Ok(resp) => {
            let status = resp.status();
            let text = crate::backend::http::error_body(resp).await;
            if let Some(error) =
                renewed_session_error(status, &token, &text, "Announce failed").await
            {
                return error;
            }
            CommandResult::http_error(
                status.as_u16(),
                format!("Announce failed ({status}): {text}"),
            )
        }
        Err(e) => CommandResult::DeliveryUnknown(format!("Announce request failed: {e}")),
    }
}

/// /shoutout <username>
pub async fn cmd_shoutout(channel: &str, args: &str) -> CommandResult {
    let username = args.trim().trim_start_matches('@');
    if username.is_empty() {
        return CommandResult::Error("Usage: /shoutout <username>".to_string());
    }

    let broadcaster_id = match get_cached_channel_id(channel) {
        Some(id) => id,
        None => return CommandResult::Error("Channel ID not found".to_string()),
    };

    let token = match get_token() {
        Some(t) => t,
        None => return CommandResult::Error("Not authenticated".to_string()),
    };

    let user_id = match get_current_user_id().await {
        Ok(id) => id,
        Err(e) => return CommandResult::Error(e),
    };

    let target_id = match get_user_id_cached(username).await {
        Ok(id) => id,
        Err(e) => {
            return CommandResult::Error(format!("Could not find user '{}': {}", username, e));
        }
    };

    let url = format!(
        "https://api.twitch.tv/helix/chat/shoutouts?from_broadcaster_id={}&to_broadcaster_id={}&moderator_id={}",
        broadcaster_id, target_id, user_id
    );

    match TWITCH_HTTP_CLIENT
        .post(&url)
        .helix_auth(&token)
        .send()
        .await
    {
        Ok(resp) if resp.status().is_success() => {
            CommandResult::Success(Some(format!("Shoutout sent to {}", username)))
        }
        Ok(resp) => {
            let status = resp.status();
            let text = crate::backend::http::error_body(resp).await;
            if let Some(error) =
                renewed_session_error(status, &token, &text, "Shoutout failed").await
            {
                return error;
            }
            CommandResult::http_error(
                status.as_u16(),
                format!("Shoutout failed ({status}): {text}"),
            )
        }
        Err(e) => CommandResult::DeliveryUnknown(format!("Shoutout request failed: {e}")),
    }
}

#[derive(Debug, Deserialize)]
struct CreateClipResponse {
    data: Vec<CreateClipEntry>,
}

#[derive(Debug, Deserialize)]
struct CreateClipEntry {
    #[serde(default)]
    id: String,
    #[serde(default)]
    edit_url: String,
}

/// /clip
pub async fn cmd_clip(channel: &str) -> CommandResult {
    let broadcaster_id = match get_cached_channel_id(channel) {
        Some(id) => id,
        None => return CommandResult::Error("Channel ID not found".to_string()),
    };

    let token = match get_token() {
        Some(t) => t,
        None => return CommandResult::Error("Not authenticated".to_string()),
    };

    let url = format!(
        "https://api.twitch.tv/helix/clips?broadcaster_id={}",
        broadcaster_id
    );

    match TWITCH_HTTP_CLIENT
        .post(&url)
        .helix_auth(&token)
        .send()
        .await
    {
        Ok(resp) if resp.status().is_success() || resp.status().as_u16() == 202 => {
            let text = crate::backend::http::bounded_body(resp).await;
            let parsed = serde_json::from_str::<CreateClipResponse>(&text).ok();
            let clip = parsed.and_then(|response| response.data.into_iter().next());

            match clip {
                Some(clip) if !clip.edit_url.trim().is_empty() => {
                    CommandResult::Success(Some(format!("Clip created: {}", clip.edit_url)))
                }
                Some(clip) if !clip.id.trim().is_empty() => {
                    CommandResult::Success(Some(format!("Clip created: {}", clip.id)))
                }
                _ => CommandResult::Success(Some("Clip created!".to_string())),
            }
        }
        Ok(resp) => {
            let status = resp.status();
            let text = crate::backend::http::error_body(resp).await;
            if let Some(error) = renewed_session_error(status, &token, &text, "Clip failed").await {
                return error;
            }
            CommandResult::http_error(status.as_u16(), format!("Clip failed ({status}): {text}"))
        }
        Err(e) => CommandResult::DeliveryUnknown(format!("Clip request failed: {e}")),
    }
}

/// /marker [description]
pub async fn cmd_marker(channel: &str, args: &str) -> CommandResult {
    let broadcaster_id = match get_channel_id_for_command(channel).await {
        Ok(id) => id,
        Err(e) => return CommandResult::Error(e),
    };

    let token = match get_token() {
        Some(t) => t,
        None => return CommandResult::Error("Not authenticated".to_string()),
    };

    let body = match super::helix_core::marker_body(&broadcaster_id, args) {
        Ok(body) => body,
        Err(usage) => return CommandResult::Error(usage),
    };

    match TWITCH_HTTP_CLIENT
        .post("https://api.twitch.tv/helix/streams/markers")
        .helix_auth(&token)
        .json(&body)
        .send()
        .await
    {
        Ok(resp) if resp.status().is_success() => {
            #[derive(Deserialize)]
            struct MarkerResponse {
                data: Vec<MarkerData>,
            }
            #[derive(Deserialize)]
            struct MarkerData {
                position_seconds: Option<u64>,
            }

            match resp.json::<MarkerResponse>().await {
                Ok(data) => {
                    let position = data
                        .data
                        .first()
                        .and_then(|marker| marker.position_seconds)
                        .map(|seconds| format!(" at {}s", seconds))
                        .unwrap_or_default();
                    CommandResult::Success(Some(format!("Stream marker created{position}")))
                }
                Err(_) => CommandResult::Success(Some("Stream marker created".to_string())),
            }
        }
        Ok(resp) => {
            let status = resp.status();
            let text = crate::backend::http::error_body(resp).await;
            if let Some(error) = renewed_session_error(status, &token, &text, "Marker failed").await
            {
                return error;
            }
            CommandResult::http_error(status.as_u16(), format!("Marker failed ({status}): {text}"))
        }
        Err(e) => CommandResult::DeliveryUnknown(format!("Marker request failed: {e}")),
    }
}

async fn modify_channel_info(
    channel: &str,
    body: serde_json::Value,
    success_message: String,
) -> CommandResult {
    let broadcaster_id = match get_channel_id_for_command(channel).await {
        Ok(id) => id,
        Err(e) => return CommandResult::Error(e),
    };

    let token = match get_token() {
        Some(t) => t,
        None => return CommandResult::Error("Not authenticated".to_string()),
    };

    let url = super::helix_core::channel_info_url(&broadcaster_id);

    match send_helix_idempotent(&token, |token| {
        TWITCH_HTTP_CLIENT.patch(&url).helix_auth(token).json(&body)
    })
    .await
    {
        Ok(resp) if resp.status().as_u16() == 204 => CommandResult::Success(Some(success_message)),
        Ok(resp) if resp.status().is_success() => CommandResult::Success(Some(success_message)),
        Ok(resp) => {
            let status = resp.status();
            let text = crate::backend::http::error_body(resp).await;
            CommandResult::http_error(
                status.as_u16(),
                format!("Channel update failed ({status}): {text}"),
            )
        }
        Err(e) => CommandResult::DeliveryUnknown(format!("Channel update request failed: {e}")),
    }
}

/// /settitle <title>
pub async fn cmd_settitle(channel: &str, args: &str) -> CommandResult {
    let body = match super::helix_core::channel_title_body(args) {
        Ok(body) => body,
        Err(usage) => return CommandResult::Error(usage),
    };
    let title = args.trim();

    modify_channel_info(
        channel,
        body,
        format!("Updated stream title to \"{title}\""),
    )
    .await
}

#[derive(Deserialize)]
struct CategorySearchResponse {
    data: Vec<CategorySearchItem>,
}

#[derive(Deserialize)]
struct CategorySearchItem {
    id: String,
    name: String,
}

const COMMERCIAL_USAGE: &str = "Usage: /commercial <30|60|90|120|150|180>";

#[derive(Deserialize)]
struct CommercialResponse {
    data: Vec<CommercialData>,
}

#[derive(Deserialize)]
struct CommercialData {
    length: u64,
    retry_after: u64,
}

#[derive(Deserialize)]
struct ShieldModeResponse {
    data: Vec<ShieldModeData>,
}

#[derive(Deserialize)]
struct ShieldModeData {
    is_active: bool,
}

async fn update_shield_mode(channel: &str, active: bool) -> CommandResult {
    let broadcaster_id = match get_channel_id_for_command(channel).await {
        Ok(id) => id,
        Err(error) => return CommandResult::Error(error),
    };
    let moderator_id = match get_current_user_id().await {
        Ok(id) => id,
        Err(error) => return CommandResult::Error(error),
    };
    let token = match get_token() {
        Some(token) => token,
        None => return CommandResult::Error("Not authenticated".to_string()),
    };
    let url = format!(
        "https://api.twitch.tv/helix/moderation/shield_mode?broadcaster_id={}&moderator_id={}",
        urlencoding::encode(&broadcaster_id),
        urlencoding::encode(&moderator_id)
    );
    let body = serde_json::json!({ "is_active": active });

    match send_helix_idempotent(&token, |token| {
        TWITCH_HTTP_CLIENT.put(&url).helix_auth(token).json(&body)
    })
    .await
    {
        Ok(resp) if resp.status().is_success() => {
            let is_active = resp
                .json::<ShieldModeResponse>()
                .await
                .ok()
                .and_then(|response| response.data.first().map(|data| data.is_active))
                .unwrap_or(active);
            if is_active {
                CommandResult::Success(Some("Shield mode was activated.".to_string()))
            } else {
                CommandResult::Success(Some("Shield mode was deactivated.".to_string()))
            }
        }
        Ok(resp) => {
            let status = resp.status();
            let text = crate::backend::http::error_body(resp).await;
            CommandResult::http_error(
                status.as_u16(),
                format!("Failed to update shield mode ({status}): {text}"),
            )
        }
        Err(error) => {
            CommandResult::DeliveryUnknown(format!("Shield mode request failed: {error}"))
        }
    }
}

/// /shield
pub async fn cmd_shield(channel: &str) -> CommandResult {
    update_shield_mode(channel, true).await
}

/// /shieldoff
pub async fn cmd_shieldoff(channel: &str) -> CommandResult {
    update_shield_mode(channel, false).await
}

const COLOR_USAGE: &str = "Usage: /color <color>";

fn clean_helix_color_name(color: &str) -> String {
    match color.trim().to_lowercase().as_str() {
        "blueviolet" => "blue_violet".to_string(),
        "cadetblue" => "cadet_blue".to_string(),
        "dodgerblue" => "dodger_blue".to_string(),
        "goldenrod" => "golden_rod".to_string(),
        "hotpink" => "hot_pink".to_string(),
        "orangered" => "orange_red".to_string(),
        "seagreen" => "sea_green".to_string(),
        "springgreen" => "spring_green".to_string(),
        "yellowgreen" => "yellow_green".to_string(),
        other => other.to_string(),
    }
}

/// /color <color>
pub async fn cmd_color(args: &str) -> CommandResult {
    let color = match args.split_whitespace().next() {
        Some(raw) if !raw.trim().is_empty() => clean_helix_color_name(raw),
        _ => return CommandResult::Error(COLOR_USAGE.to_string()),
    };
    let user_id = match get_current_user_id().await {
        Ok(id) => id,
        Err(error) => return CommandResult::Error(error),
    };
    let token = match get_token() {
        Some(token) => token,
        None => return CommandResult::Error("Not authenticated".to_string()),
    };
    let body = serde_json::json!({
        "user_id": user_id,
        "color": color,
    });

    match send_helix_idempotent(&token, |token| {
        TWITCH_HTTP_CLIENT
            .put("https://api.twitch.tv/helix/chat/color")
            .helix_auth(token)
            .json(&body)
    })
    .await
    {
        Ok(resp) if resp.status().is_success() => {
            CommandResult::Success(Some(format!("Your color has been changed to {color}.")))
        }
        Ok(resp) => {
            let status = resp.status();
            let text = crate::backend::http::error_body(resp).await;
            CommandResult::http_error(
                status.as_u16(),
                format!("Failed to change color to {color} ({status}): {text}"),
            )
        }
        Err(error) => CommandResult::DeliveryUnknown(format!("Color request failed: {error}")),
    }
}

/// /commercial <length>
pub async fn cmd_commercial(channel: &str, args: &str) -> CommandResult {
    let length = match args.split_whitespace().next() {
        Some(raw) => match raw.parse::<u64>() {
            Ok(length) if matches!(length, 30 | 60 | 90 | 120 | 150 | 180) => length,
            _ => return CommandResult::Error(COMMERCIAL_USAGE.to_string()),
        },
        None => return CommandResult::Error(COMMERCIAL_USAGE.to_string()),
    };

    let broadcaster_id = match get_channel_id_for_command(channel).await {
        Ok(id) => id,
        Err(error) => return CommandResult::Error(error),
    };
    let token = match get_token() {
        Some(token) => token,
        None => return CommandResult::Error("Not authenticated".to_string()),
    };
    let body = serde_json::json!({
        "broadcaster_id": broadcaster_id,
        "length": length,
    });

    match TWITCH_HTTP_CLIENT
        .post("https://api.twitch.tv/helix/channels/commercial")
        .helix_auth(&token)
        .json(&body)
        .send()
        .await
    {
        Ok(resp) if resp.status().is_success() => match resp.json::<CommercialResponse>().await {
            Ok(response) => match response.data.first() {
                Some(data) => CommandResult::Success(Some(format!(
                    "Starting {} second long commercial break. You may run another commercial in {} seconds.",
                    data.length, data.retry_after
                ))),
                None => CommandResult::Success(Some("Commercial started".to_string())),
            },
            Err(_) => CommandResult::Success(Some("Commercial started".to_string())),
        },
        Ok(resp) => {
            let status = resp.status();
            let text = crate::backend::http::error_body(resp).await;
            if let Some(error) =
                renewed_session_error(status, &token, &text, "Commercial failed").await
            {
                return error;
            }
            CommandResult::http_error(
                status.as_u16(),
                format!("Commercial failed ({status}): {text}"),
            )
        }
        Err(error) => CommandResult::DeliveryUnknown(format!("Commercial request failed: {error}")),
    }
}

/// /setgame <game>
pub async fn cmd_setgame(channel: &str, args: &str) -> CommandResult {
    let url = match super::helix_core::category_search_url(args) {
        Ok(url) => url,
        Err(usage) => return CommandResult::Error(usage),
    };

    let token = match get_token() {
        Some(t) => t,
        None => return CommandResult::Error("Not authenticated".to_string()),
    };

    let category = match send_helix_idempotent(&token, |token| {
        TWITCH_HTTP_CLIENT.get(&url).helix_auth(token)
    })
    .await
    {
        Ok(resp) if resp.status().is_success() => match resp.json::<CategorySearchResponse>().await
        {
            Ok(mut result) => match result.data.pop() {
                Some(category) => category,
                None => {
                    return CommandResult::Error(format!("No game found for \"{}\"", args.trim()));
                }
            },
            Err(e) => return CommandResult::Error(format!("Failed to parse category search: {e}")),
        },
        Ok(resp) => {
            let status = resp.status();
            let text = crate::backend::http::error_body(resp).await;
            return CommandResult::Error(format!("Category search failed ({}): {}", status, text));
        }
        Err(e) => return CommandResult::Error(format!("Category search request failed: {e}")),
    };

    let body = match super::helix_core::channel_game_body(&category.id) {
        Ok(body) => body,
        Err(usage) => return CommandResult::Error(usage),
    };

    modify_channel_info(
        channel,
        body,
        format!("Updated stream category to {}", category.name),
    )
    .await
}

/// /raid <channel>
pub async fn cmd_raid(channel: &str, args: &str) -> CommandResult {
    let target = args.trim().trim_start_matches('@');
    if target.is_empty() {
        return CommandResult::Error("Usage: /raid <channel>".to_string());
    }

    let broadcaster_id = match get_cached_channel_id(channel) {
        Some(id) => id,
        None => return CommandResult::Error("Channel ID not found".to_string()),
    };

    let token = match get_token() {
        Some(t) => t,
        None => return CommandResult::Error("Not authenticated".to_string()),
    };

    let target_id = match get_user_id_cached(target).await {
        Ok(id) => id,
        Err(e) => return CommandResult::Error(format!("Could not find user '{}': {}", target, e)),
    };

    let url = format!(
        "https://api.twitch.tv/helix/raids?from_broadcaster_id={}&to_broadcaster_id={}",
        broadcaster_id, target_id
    );

    match send_helix_idempotent(&token, |token| {
        TWITCH_HTTP_CLIENT.post(&url).helix_auth(token)
    })
    .await
    {
        Ok(resp) if resp.status().is_success() => {
            CommandResult::Success(Some(format!("Raiding {}", target)))
        }
        Ok(resp) => {
            let status = resp.status();
            let text = crate::backend::http::error_body(resp).await;
            CommandResult::http_error(status.as_u16(), format!("Raid failed ({status}): {text}"))
        }
        Err(e) => CommandResult::DeliveryUnknown(format!("Raid request failed: {e}")),
    }
}

/// /mod <username>
pub async fn cmd_mod(channel: &str, args: &str) -> CommandResult {
    modify_channel_role(channel, args, ChannelRoleAction::AddModerator).await
}

pub(crate) async fn cmd_mod_with_auth(
    channel: &str,
    args: &str,
    auth: &crate::backend::auth::AuthenticatedUserResource,
) -> CommandResult {
    modify_channel_role_with_auth(channel, args, ChannelRoleAction::AddModerator, auth).await
}

/// /unmod <username>
pub async fn cmd_unmod(channel: &str, args: &str) -> CommandResult {
    modify_channel_role(channel, args, ChannelRoleAction::RemoveModerator).await
}

pub(crate) async fn cmd_unmod_with_auth(
    channel: &str,
    args: &str,
    auth: &crate::backend::auth::AuthenticatedUserResource,
) -> CommandResult {
    modify_channel_role_with_auth(channel, args, ChannelRoleAction::RemoveModerator, auth).await
}

/// /vip <username>
pub async fn cmd_vip(channel: &str, args: &str) -> CommandResult {
    modify_channel_role(channel, args, ChannelRoleAction::AddVip).await
}

pub(crate) async fn cmd_vip_with_auth(
    channel: &str,
    args: &str,
    auth: &crate::backend::auth::AuthenticatedUserResource,
) -> CommandResult {
    modify_channel_role_with_auth(channel, args, ChannelRoleAction::AddVip, auth).await
}

/// /unvip <username>
pub async fn cmd_unvip(channel: &str, args: &str) -> CommandResult {
    modify_channel_role(channel, args, ChannelRoleAction::RemoveVip).await
}

pub(crate) async fn cmd_unvip_with_auth(
    channel: &str,
    args: &str,
    auth: &crate::backend::auth::AuthenticatedUserResource,
) -> CommandResult {
    modify_channel_role_with_auth(channel, args, ChannelRoleAction::RemoveVip, auth).await
}

/// /warn <username> <reason>
pub async fn cmd_warn(channel: &str, args: &str) -> CommandResult {
    let auth = match capture_moderation_auth() {
        Ok(auth) => auth,
        Err(error) => return error,
    };
    cmd_warn_with_auth(channel, args, &auth).await
}

pub(crate) async fn cmd_warn_with_auth(
    channel: &str,
    args: &str,
    auth: &crate::backend::auth::AuthenticatedUserResource,
) -> CommandResult {
    let mut parts = args.splitn(2, char::is_whitespace);
    let username = match parts.next() {
        Some(u) if !u.is_empty() => u.trim_start_matches('@'),
        _ => return CommandResult::Error("Usage: /warn <username> <reason>".to_string()),
    };
    let reason = parts.next().unwrap_or("").trim();
    if reason.is_empty() {
        return CommandResult::Error("Usage: /warn <username> <reason>".to_string());
    }

    let (broadcaster_id, user_id, token) =
        match helix_moderation_context_with_auth(channel, auth).await {
            Ok(context) => context,
            Err(error) => return CommandResult::Error(error),
        };

    let target_id = match get_user_id_cached(username).await {
        Ok(id) => id,
        Err(e) => {
            return CommandResult::Error(format!("Could not find user '{}': {}", username, e));
        }
    };

    let url = format!(
        "https://api.twitch.tv/helix/moderation/warnings?broadcaster_id={}&moderator_id={}",
        broadcaster_id, user_id
    );

    let body = serde_json::json!({
        "data": {
            "user_id": target_id,
            "reason": reason
        }
    });

    if !auth.is_current() {
        return CommandResult::Error(
            "The active Twitch account changed before the moderation request was sent. Try again."
                .to_string(),
        );
    }
    match TWITCH_HTTP_CLIENT
        .post(&url)
        .helix_auth(&token)
        .json(&body)
        .send()
        .await
    {
        Ok(resp) if resp.status().is_success() => {
            CommandResult::Success(Some(format!("Warning sent to {}", username)))
        }
        Ok(resp) => {
            let status = resp.status();
            let text = crate::backend::http::error_body(resp).await;
            if let Some(error) = renewed_session_error(status, &token, &text, "Warn failed").await {
                return error;
            }
            CommandResult::http_error(status.as_u16(), format!("Warn failed ({status}): {text}"))
        }
        Err(e) => CommandResult::DeliveryUnknown(format!("Warn request failed: {e}")),
    }
}

/// A Helix 401 for a request that is not re-sent: renew the session so the next
/// attempt succeeds and report that instead of the raw rejection.
async fn renewed_session_error(
    status: reqwest::StatusCode,
    token: &str,
    detail: &str,
    failure: &str,
) -> Option<CommandResult> {
    (status == reqwest::StatusCode::UNAUTHORIZED && renew_helix_session(token, detail).await).then(
        || {
            CommandResult::Error(format!(
                "{failure}: the Twitch session had expired and was renewed. Try again."
            ))
        },
    )
}

/// Friendly authentication blockers for Helix command paths, mirroring the
/// `ensure_authenticated_send_ready` wording used by the IRC send path.
fn ensure_helix_command_token() -> Result<String, String> {
    if crate::backend::irc::is_anonymous_mode() {
        return Err(
            "Cannot run Twitch commands while Echo is connected anonymously. Turn off Anonymous in the account switcher, re-authenticate the active Twitch account, or wait for the authenticated reconnect to finish."
                .to_string(),
        );
    }

    if crate::backend::auth::get_needs_reauth() {
        return Err(
            "Cannot run Twitch commands until the active Twitch account is re-authenticated."
                .to_string(),
        );
    }

    let Some(username) = get_username().filter(|username| !username.trim().is_empty()) else {
        return Err("Cannot run Twitch commands without an active Twitch account.".to_string());
    };

    match get_token().filter(|token| !token.trim().is_empty()) {
        Some(token) => Ok(token),
        None => Err(format!(
            "Cannot run Twitch commands as {username}; the active account has no Twitch token. Re-authenticate the account."
        )),
    }
}

/// Resolve (broadcaster_id, moderator_id, token) for moderator-scoped Helix calls.
async fn helix_moderation_context(channel: &str) -> Result<(String, String, String), String> {
    let token = ensure_helix_command_token()?;
    let broadcaster_id = get_channel_id_for_command(channel).await?;
    let moderator_id = get_current_user_id().await?;
    Ok((broadcaster_id, moderator_id, token))
}

/// Resolve moderation request inputs from one account-generation snapshot. The
/// captured token/user ID must never be mixed with later account state.
async fn helix_moderation_context_with_auth(
    channel: &str,
    auth: &crate::backend::auth::AuthenticatedUserResource,
) -> Result<(String, String, String), String> {
    if !auth.is_current() {
        return Err(
            "The active Twitch account changed before moderation could be updated. Try again."
                .to_string(),
        );
    }
    let broadcaster_id = get_channel_id_for_command(channel).await?;
    if !auth.is_current() {
        return Err(
            "The active Twitch account changed before moderation could be updated. Try again."
                .to_string(),
        );
    }
    Ok((
        broadcaster_id,
        auth.user_id.clone(),
        auth.access_token.clone(),
    ))
}

fn capture_moderation_auth()
-> Result<crate::backend::auth::AuthenticatedUserResource, CommandResult> {
    crate::backend::auth::capture_authenticated_user_resource().ok_or_else(|| {
        CommandResult::Error(
            "Cannot run moderation commands until the active Twitch account identity finishes loading."
                .to_string(),
        )
    })
}

fn channel_label(channel: &str) -> &str {
    channel.trim_start_matches('#')
}

async fn apply_ban_state_by_user_id_with_auth(
    channel: &str,
    user_id: &str,
    duration_seconds: Option<u64>,
    reason: Option<&str>,
    target_label: &str,
    auth: &crate::backend::auth::AuthenticatedUserResource,
) -> CommandResult {
    let target_id = user_id.trim();
    if target_id.is_empty() {
        return CommandResult::Error("Usage: id:<userID>".to_string());
    }

    let (broadcaster_id, moderator_id, token) =
        match helix_moderation_context_with_auth(channel, auth).await {
            Ok(context) => context,
            Err(error) => return CommandResult::Error(error),
        };
    let url = format!(
        "https://api.twitch.tv/helix/moderation/bans?broadcaster_id={}&moderator_id={}",
        urlencoding::encode(&broadcaster_id),
        urlencoding::encode(&moderator_id)
    );
    let mut data = serde_json::Map::new();
    data.insert("user_id".to_string(), serde_json::json!(target_id));
    if let Some(duration) = duration_seconds {
        data.insert("duration".to_string(), serde_json::json!(duration));
    }
    if let Some(reason) = reason.filter(|reason| !reason.trim().is_empty()) {
        data.insert("reason".to_string(), serde_json::json!(reason.trim()));
    }
    let body = serde_json::json!({ "data": data });

    let channel = channel_label(channel);
    if !auth.is_current() {
        return CommandResult::Error(
            "The active Twitch account changed before the moderation request was sent. Try again."
                .to_string(),
        );
    }
    match send_helix_idempotent(&token, |token| {
        TWITCH_HTTP_CLIENT.post(&url).helix_auth(token).json(&body)
    })
    .await
    {
        Ok(resp) if resp.status().is_success() => match duration_seconds {
            Some(duration) => CommandResult::Success(Some(format!(
                "Timed out {target_label} in #{channel} for {duration}s"
            ))),
            None => CommandResult::Success(Some(format!("Banned {target_label} in #{channel}"))),
        },
        Ok(resp) => {
            let status = resp.status();
            let text = crate::backend::http::error_body(resp).await;
            let action = if duration_seconds.is_some() {
                "timeout"
            } else {
                "ban"
            };
            CommandResult::http_error(
                status.as_u16(),
                format!("Failed to {action} {target_label} ({status}): {text}"),
            )
        }
        Err(error) => CommandResult::DeliveryUnknown(format!("Moderation request failed: {error}")),
    }
}

async fn remove_ban_state_by_user_id_with_auth(
    channel: &str,
    user_id: &str,
    target_label: &str,
    auth: &crate::backend::auth::AuthenticatedUserResource,
) -> CommandResult {
    let target_id = user_id.trim();
    if target_id.is_empty() {
        return CommandResult::Error("Usage: id:<userID>".to_string());
    }

    let (broadcaster_id, moderator_id, token) =
        match helix_moderation_context_with_auth(channel, auth).await {
            Ok(context) => context,
            Err(error) => return CommandResult::Error(error),
        };
    let url = format!(
        "https://api.twitch.tv/helix/moderation/bans?broadcaster_id={}&moderator_id={}&user_id={}",
        urlencoding::encode(&broadcaster_id),
        urlencoding::encode(&moderator_id),
        urlencoding::encode(target_id)
    );

    let channel = channel_label(channel);
    if !auth.is_current() {
        return CommandResult::Error(
            "The active Twitch account changed before the moderation request was sent. Try again."
                .to_string(),
        );
    }
    match send_helix_idempotent(&token, |token| {
        TWITCH_HTTP_CLIENT.delete(&url).helix_auth(token)
    })
    .await
    {
        Ok(resp) if resp.status().is_success() => CommandResult::Success(Some(format!(
            "Removed ban or timeout for {target_label} in #{channel}"
        ))),
        Ok(resp) => {
            let status = resp.status();
            let text = crate::backend::http::error_body(resp).await;
            CommandResult::http_error(
                status.as_u16(),
                format!("Failed to remove ban or timeout for {target_label} ({status}): {text}"),
            )
        }
        Err(error) => CommandResult::DeliveryUnknown(format!("Unban request failed: {error}")),
    }
}

pub(crate) async fn cmd_ban_user_id_with_auth(
    channel: &str,
    user_id: &str,
    reason: Option<&str>,
    auth: &crate::backend::auth::AuthenticatedUserResource,
) -> CommandResult {
    let target_label = format!("user ID {}", user_id.trim());
    apply_ban_state_by_user_id_with_auth(channel, user_id, None, reason, &target_label, auth).await
}

pub(crate) async fn cmd_timeout_user_id_with_auth(
    channel: &str,
    user_id: &str,
    duration_seconds: u64,
    reason: Option<&str>,
    auth: &crate::backend::auth::AuthenticatedUserResource,
) -> CommandResult {
    let target_label = format!("user ID {}", user_id.trim());
    apply_ban_state_by_user_id_with_auth(
        channel,
        user_id,
        Some(duration_seconds),
        reason,
        &target_label,
        auth,
    )
    .await
}

pub(crate) async fn cmd_unban_user_id_with_auth(
    channel: &str,
    user_id: &str,
    auth: &crate::backend::auth::AuthenticatedUserResource,
) -> CommandResult {
    let target_label = format!("user ID {}", user_id.trim());
    remove_ban_state_by_user_id_with_auth(channel, user_id, &target_label, auth).await
}

async fn resolve_target_user_id(username: &str) -> Result<String, String> {
    get_user_id_cached(username)
        .await
        .map_err(|error| format!("Could not find user '{username}': {error}"))
}

pub(crate) async fn cmd_timeout_username_with_auth(
    channel: &str,
    username: &str,
    duration_seconds: u64,
    reason: Option<&str>,
    auth: &crate::backend::auth::AuthenticatedUserResource,
) -> CommandResult {
    let username = username.trim().trim_start_matches('@');
    let user_id = match resolve_target_user_id(username).await {
        Ok(id) => id,
        Err(error) => return CommandResult::Error(error),
    };
    apply_ban_state_by_user_id_with_auth(
        channel,
        &user_id,
        Some(duration_seconds),
        reason,
        username,
        auth,
    )
    .await
}

pub(crate) async fn cmd_ban_username_with_auth(
    channel: &str,
    username: &str,
    reason: Option<&str>,
    auth: &crate::backend::auth::AuthenticatedUserResource,
) -> CommandResult {
    let username = username.trim().trim_start_matches('@');
    let user_id = match resolve_target_user_id(username).await {
        Ok(id) => id,
        Err(error) => return CommandResult::Error(error),
    };
    apply_ban_state_by_user_id_with_auth(channel, &user_id, None, reason, username, auth).await
}

pub(crate) async fn cmd_unban_username_with_auth(
    channel: &str,
    username: &str,
    auth: &crate::backend::auth::AuthenticatedUserResource,
) -> CommandResult {
    let username = username.trim().trim_start_matches('@');
    let user_id = match resolve_target_user_id(username).await {
        Ok(id) => id,
        Err(error) => return CommandResult::Error(error),
    };
    remove_ban_state_by_user_id_with_auth(channel, &user_id, username, auth).await
}

/// DELETE /helix/moderation/chat — delete one message, or clear chat when
/// `message_id` is `None`. Requires moderator:manage:chat_messages.
pub async fn delete_chat_messages(channel: &str, message_id: Option<&str>) -> CommandResult {
    let auth = match capture_moderation_auth() {
        Ok(auth) => auth,
        Err(error) => return error,
    };
    delete_chat_messages_with_auth(channel, message_id, &auth).await
}

pub(crate) async fn delete_chat_messages_with_auth(
    channel: &str,
    message_id: Option<&str>,
    auth: &crate::backend::auth::AuthenticatedUserResource,
) -> CommandResult {
    let (broadcaster_id, moderator_id, token) =
        match helix_moderation_context_with_auth(channel, auth).await {
            Ok(context) => context,
            Err(error) => return CommandResult::Error(error),
        };
    let mut url = format!(
        "https://api.twitch.tv/helix/moderation/chat?broadcaster_id={}&moderator_id={}",
        urlencoding::encode(&broadcaster_id),
        urlencoding::encode(&moderator_id)
    );
    if let Some(id) = message_id {
        url.push_str("&message_id=");
        url.push_str(&urlencoding::encode(id));
    }

    let channel = channel_label(channel);
    if !auth.is_current() {
        return CommandResult::Error(
            "The active Twitch account changed before the moderation request was sent. Try again."
                .to_string(),
        );
    }
    match send_helix_idempotent(&token, |token| {
        TWITCH_HTTP_CLIENT.delete(&url).helix_auth(token)
    })
    .await
    {
        Ok(resp) if resp.status().is_success() => match message_id {
            Some(id) => CommandResult::Success(Some(format!("Deleted message {id} in #{channel}"))),
            None => CommandResult::Success(Some(format!("Cleared chat in #{channel}"))),
        },
        Ok(resp) => {
            let status = resp.status();
            let text = crate::backend::http::error_body(resp).await;
            let action = if message_id.is_some() {
                "delete message"
            } else {
                "clear chat"
            };
            CommandResult::http_error(
                status.as_u16(),
                format!("Failed to {action} in #{channel} ({status}): {text}"),
            )
        }
        Err(error) => CommandResult::DeliveryUnknown(format!("Moderation request failed: {error}")),
    }
}

/// PATCH /helix/chat/settings with only the changed fields.
/// Requires moderator:manage:chat_settings.
async fn update_chat_settings(
    channel: &str,
    patch: serde_json::Value,
    success_message: String,
) -> CommandResult {
    let (broadcaster_id, moderator_id, token) = match helix_moderation_context(channel).await {
        Ok(context) => context,
        Err(error) => return CommandResult::Error(error),
    };
    let url = format!(
        "https://api.twitch.tv/helix/chat/settings?broadcaster_id={}&moderator_id={}",
        urlencoding::encode(&broadcaster_id),
        urlencoding::encode(&moderator_id)
    );

    match send_helix_idempotent(&token, |token| {
        TWITCH_HTTP_CLIENT
            .patch(&url)
            .helix_auth(token)
            .json(&patch)
    })
    .await
    {
        Ok(resp) if resp.status().is_success() => CommandResult::Success(Some(success_message)),
        Ok(resp) => {
            let status = resp.status();
            let text = crate::backend::http::error_body(resp).await;
            CommandResult::http_error(
                status.as_u16(),
                format!(
                    "Failed to update chat settings in #{} ({status}): {text}",
                    channel_label(channel)
                ),
            )
        }
        Err(error) => {
            CommandResult::DeliveryUnknown(format!("Chat settings request failed: {error}"))
        }
    }
}

/// /emoteonly and /emoteonlyoff
pub async fn cmd_emote_only(channel: &str, enabled: bool) -> CommandResult {
    let state = if enabled { "Enabled" } else { "Disabled" };
    update_chat_settings(
        channel,
        serde_json::json!({ "emote_mode": enabled }),
        format!("{state} emote-only mode in #{}", channel_label(channel)),
    )
    .await
}

/// /subscribers and /subscribersoff
pub async fn cmd_subscribers_only(channel: &str, enabled: bool) -> CommandResult {
    let state = if enabled { "Enabled" } else { "Disabled" };
    update_chat_settings(
        channel,
        serde_json::json!({ "subscriber_mode": enabled }),
        format!(
            "{state} subscribers-only mode in #{}",
            channel_label(channel)
        ),
    )
    .await
}

/// /uniquechat, /r9kbeta and their off variants
pub async fn cmd_unique_chat(channel: &str, enabled: bool) -> CommandResult {
    let state = if enabled { "Enabled" } else { "Disabled" };
    update_chat_settings(
        channel,
        serde_json::json!({ "unique_chat_mode": enabled }),
        format!("{state} unique-chat mode in #{}", channel_label(channel)),
    )
    .await
}

const DEFAULT_SLOW_MODE_SECONDS: u64 = 30;
const MIN_SLOW_MODE_SECONDS: u64 = 3;
const MAX_SLOW_MODE_SECONDS: u64 = 120;
const MAX_FOLLOWER_MODE_MINUTES: u64 = 129_600;

/// /slow [seconds]
pub async fn cmd_slow_mode(channel: &str, args: &str) -> CommandResult {
    let args = args.trim();
    let seconds = if args.is_empty() {
        DEFAULT_SLOW_MODE_SECONDS
    } else {
        match args.parse::<u64>() {
            Ok(seconds) => seconds,
            Err(_) => return CommandResult::Error("Usage: /slow [seconds]".to_string()),
        }
    };
    if !(MIN_SLOW_MODE_SECONDS..=MAX_SLOW_MODE_SECONDS).contains(&seconds) {
        return CommandResult::Error(format!(
            "Slow-mode wait time must be between {MIN_SLOW_MODE_SECONDS} and {MAX_SLOW_MODE_SECONDS} seconds"
        ));
    }
    update_chat_settings(
        channel,
        serde_json::json!({ "slow_mode": true, "slow_mode_wait_time": seconds }),
        format!(
            "Enabled {seconds}s slow mode in #{}",
            channel_label(channel)
        ),
    )
    .await
}

/// /slowoff
pub async fn cmd_slow_mode_off(channel: &str) -> CommandResult {
    update_chat_settings(
        channel,
        serde_json::json!({ "slow_mode": false }),
        format!("Disabled slow mode in #{}", channel_label(channel)),
    )
    .await
}

/// /followers [minutes]
pub async fn cmd_followers_only(channel: &str, args: &str) -> CommandResult {
    let args = args.trim();
    let minutes = if args.is_empty() {
        0
    } else {
        match args.parse::<u64>() {
            Ok(minutes) => minutes,
            Err(_) => return CommandResult::Error("Usage: /followers [minutes]".to_string()),
        }
    };
    if minutes > MAX_FOLLOWER_MODE_MINUTES {
        return CommandResult::Error(format!(
            "Followers-only duration must be between 0 and {MAX_FOLLOWER_MODE_MINUTES} minutes"
        ));
    }
    let success_message = if minutes == 0 {
        format!(
            "Enabled followers-only mode in #{} (any follower)",
            channel_label(channel)
        )
    } else {
        format!(
            "Enabled followers-only mode in #{} ({minutes} minute follow requirement)",
            channel_label(channel)
        )
    };
    update_chat_settings(
        channel,
        serde_json::json!({ "follower_mode": true, "follower_mode_duration": minutes }),
        success_message,
    )
    .await
}

/// /followersoff
pub async fn cmd_followers_only_off(channel: &str) -> CommandResult {
    update_chat_settings(
        channel,
        serde_json::json!({ "follower_mode": false }),
        format!(
            "Disabled followers-only mode in #{}",
            channel_label(channel)
        ),
    )
    .await
}

/// /unraid
pub async fn cmd_unraid(channel: &str) -> CommandResult {
    let token = match ensure_helix_command_token() {
        Ok(token) => token,
        Err(error) => return CommandResult::Error(error),
    };
    let broadcaster_id = match get_channel_id_for_command(channel).await {
        Ok(id) => id,
        Err(error) => return CommandResult::Error(error),
    };

    let url = format!(
        "https://api.twitch.tv/helix/raids?broadcaster_id={}",
        urlencoding::encode(&broadcaster_id)
    );

    match send_helix_idempotent(&token, |token| {
        TWITCH_HTTP_CLIENT.delete(&url).helix_auth(token)
    })
    .await
    {
        Ok(resp) if resp.status().is_success() => CommandResult::Success(Some(format!(
            "Canceled the raid from #{}",
            channel_label(channel)
        ))),
        Ok(resp) => {
            let status = resp.status();
            let text = crate::backend::http::error_body(resp).await;
            CommandResult::http_error(status.as_u16(), format!("Unraid failed ({status}): {text}"))
        }
        Err(e) => CommandResult::DeliveryUnknown(format!("Unraid request failed: {e}")),
    }
}

async fn set_user_block_state(args: &str, block: bool) -> CommandResult {
    let usage = if block {
        "Usage: /block <user>"
    } else {
        "Usage: /unblock <user>"
    };
    let username = match super::twitch_core::parse_single_username(args, usage) {
        Ok(username) => username,
        Err(usage) => return CommandResult::Error(usage.to_string()),
    };
    let session = match crate::backend::blocked_users::capture_active_session() {
        Ok(session) => session,
        Err(error) => return CommandResult::Error(error),
    };
    let target_id = match resolve_target_user_id(username).await {
        Ok(id) => id,
        Err(error) => return CommandResult::Error(error),
    };
    let user = crate::backend::blocked_users::BlockedUser {
        user_id: target_id,
        user_login: username.to_string(),
        display_name: username.to_string(),
    };

    match crate::backend::blocked_users::set_block_state_for_session(session, user, block).await {
        Ok(()) => {
            let verb = if block { "Blocked" } else { "Unblocked" };
            CommandResult::Success(Some(format!("{verb} {username} on Twitch")))
        }
        Err(error) => CommandResult::Error(error),
    }
}

/// /block <user>
pub async fn cmd_block(args: &str) -> CommandResult {
    set_user_block_state(args, true).await
}

/// /unblock <user>
pub async fn cmd_unblock(args: &str) -> CommandResult {
    set_user_block_state(args, false).await
}

/// /ignore <user> — alias for /block
pub async fn cmd_ignore(args: &str) -> CommandResult {
    match cmd_block(args).await {
        CommandResult::Success(_) => CommandResult::Success(Some(
            "Ignore command has been renamed to /block.".to_string(),
        )),
        result => result,
    }
}

/// /unignore <user> — alias for /unblock
pub async fn cmd_unignore(args: &str) -> CommandResult {
    match cmd_unblock(args).await {
        CommandResult::Success(_) => CommandResult::Success(Some(
            "Unignore command has been renamed to /unblock.".to_string(),
        )),
        result => result,
    }
}

/// /monitor <username>
pub async fn cmd_monitor(channel: &str, args: &str) -> CommandResult {
    if args.trim().is_empty() {
        return CommandResult::Error("Usage: /monitor <username>".to_string());
    }
    manage_suspicious_user_status(channel, args, None, SuspiciousUserAction::Monitor).await
}

/// /unmonitor <username>
pub async fn cmd_unmonitor(channel: &str, args: &str) -> CommandResult {
    if args.trim().is_empty() {
        return CommandResult::Error("Usage: /unmonitor <username>".to_string());
    }
    manage_suspicious_user_status(channel, args, None, SuspiciousUserAction::Clear).await
}

/// /restrict <username>
pub async fn cmd_restrict(channel: &str, args: &str) -> CommandResult {
    if args.trim().is_empty() {
        return CommandResult::Error("Usage: /restrict <username>".to_string());
    }
    manage_suspicious_user_status(channel, args, None, SuspiciousUserAction::Restrict).await
}

/// /unrestrict <username>
pub async fn cmd_unrestrict(channel: &str, args: &str) -> CommandResult {
    if args.trim().is_empty() {
        return CommandResult::Error("Usage: /unrestrict <username>".to_string());
    }
    manage_suspicious_user_status(channel, args, None, SuspiciousUserAction::Clear).await
}
