//! IRC/Chat commands and message retrieval
//!
//! Handles channel connections, message sending, and message queries.

use super::{ChatMessage, RUNTIME, StreamInfo, block_on_safe};
use crate::backend::ui_actions::{PendingFailedSend, SendDraftOrigin, push_pending_failed_send};
use dashmap::DashMap;
use futures_util::FutureExt;
use std::collections::{HashMap, HashSet, VecDeque};
use std::panic::AssertUnwindSafe;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock};

/// Connect to a Twitch channel
/// Uses immediate join when IRC is already connected (bypasses 5-second poll)
pub fn connect_channel(channel: &str) {
    if crate::backend::whispers::is_whispers_channel(channel) {
        crate::backend::irc::CHAT_MESSAGES
            .entry(crate::backend::whispers::WHISPERS_CHANNEL.to_string())
            .or_insert_with(|| {
                crate::state::MessageBuffer::new(crate::backend::whispers::MAX_WHISPER_MESSAGES)
            });
        return;
    }
    crate::backend::irc::note_channel_opened(channel);
    if crate::bench::network_lanes_suppressed() {
        crate::log_irc!(
            debug,
            "Offline benchmark suppressed channel connection for {}",
            channel
        );
        return;
    }
    let channel = channel.to_string();
    crate::backend::emotes::activate_channel_emotes(&channel);
    crate::backend::eventsub::retain_open_channel(&channel);

    let immediate_success = crate::backend::irc::join_channel_immediately(&channel);

    if immediate_success {
        crate::log_irc!(
            info,
            "Immediate join succeeded for {}, spawning history + EventSub in parallel",
            channel
        );
        let channel_for_history = channel.clone();
        let channel_for_eventsub = channel.clone();

        RUNTIME.spawn(async move {
            crate::backend::irc::spawn_history_load(channel_for_history).await;
        });
        RUNTIME.spawn(async move {
            setup_eventsub_for_channel(&channel_for_eventsub).await;
        });
        return;
    }

    crate::log_irc!(
        debug,
        "Immediate join not available, using async connection for {}",
        channel
    );
    RUNTIME.spawn(async move {
        crate::backend::irc::connect_and_listen(channel.clone()).await;

        setup_eventsub_for_channel(&channel).await;
    });
}

/// Set up EventSub subscription for a channel (extracted for reuse)
async fn setup_eventsub_for_channel(channel: &str) {
    if crate::backend::settings::get_token().is_some() {
        crate::backend::auth::wait_for_auth_initialization().await;
    }

    let token = crate::backend::settings::get_token();
    crate::log_net!(
        debug,
        "EventSub check for {}: token present = {}",
        channel,
        token.is_some()
    );

    if let Some(token) = token {
        crate::log_net!(debug, "Getting broadcaster ID for {}", channel);
        match crate::backend::twitch_api::get_user_id_helix(channel, &token).await {
            Ok(broadcaster_id) => {
                crate::log_net!(
                    info,
                    "Subscribing to EventSub for channel {} (id: {})",
                    channel,
                    broadcaster_id
                );
                crate::backend::eventsub::subscribe_channel(channel, &broadcaster_id);
            }
            Err(e) => {
                crate::log_net!(
                    warn,
                    "Failed to get channel ID for EventSub subscription: {}",
                    e
                );
            }
        }
    }
}

struct ComposerSendCompletion {
    channel: String,
    action: &'static str,
    origin: Option<SendDraftOrigin>,
}

impl ComposerSendCompletion {
    fn new(channel: &str, action: &'static str, origin: Option<SendDraftOrigin>) -> Self {
        Self {
            channel: channel.to_string(),
            action,
            origin,
        }
    }

    fn accepted(&mut self, unconfirmed: bool) {
        if let Some(origin) = self.origin.take() {
            crate::backend::ui_actions::push_pending_completed_send(
                crate::backend::ui_actions::PendingCompletedSend {
                    origin,
                    channel: self.channel.clone(),
                    notice: unconfirmed.then(|| "Not confirmed — check chat".to_string()),
                },
            );
        }
    }

    fn failed(&mut self, error: &crate::backend::irc::SendError) {
        report_send_failure(&self.channel, self.action, error, self.origin.take());
    }
}

impl Drop for ComposerSendCompletion {
    fn drop(&mut self) {
        if let Some(origin) = self.origin.take() {
            push_pending_failed_send(PendingFailedSend {
                origin,
                channel: self.channel.clone(),
                action: self.action,
                error: "The send ended without confirmation — check chat".to_string(),
                delivery_unknown: true,
            });
        }
    }
}

fn unconfirmed_action(message: &str) -> bool {
    message
        .get(..4)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("/me "))
}

fn publish_send_failure(channel: &str, action: &str, error: &str, delivery_unknown: bool) {
    let outcome = if delivery_unknown {
        "not confirmed"
    } else {
        "not sent"
    };
    publish_command_error(channel, &format!("{action} {outcome}"), error);
}

/// Hand a rejected composer send back to its split, or post the error row
/// directly when the send did not come from a composer draft.
fn report_send_failure(
    channel: &str,
    action: &'static str,
    error: &crate::backend::irc::SendError,
    origin: Option<SendDraftOrigin>,
) {
    match origin {
        Some(origin) => push_pending_failed_send(PendingFailedSend {
            origin,
            channel: channel.to_string(),
            action,
            error: error.message.clone(),
            delivery_unknown: error.delivery_unknown,
        }),
        None => publish_send_failure(channel, action, &error.message, error.delivery_unknown),
    }
}

/// Error row for a rejected composer send; `recall_hint` points at input
/// history when the text could not go back into the draft.
pub fn publish_failed_send_row(
    channel: &str,
    action: &str,
    error: &str,
    recall_hint: bool,
    delivery_unknown: bool,
) {
    if recall_hint {
        let detail = trim_error_detail(error, 1024, "No error details were provided.");
        publish_command_error(
            channel,
            &format!(
                "{action} {}",
                if delivery_unknown {
                    "not confirmed"
                } else {
                    "not sent"
                }
            ),
            &format!("{detail} (\u{2191} to recall)"),
        );
    } else {
        publish_send_failure(channel, action, error, delivery_unknown);
    }
}

/// Keep command/send failures in the originating chat so they remain readable
/// after a transient notification would have disappeared.
pub(super) fn publish_command_error(channel: &str, action: &str, error: &str) {
    let text = format!(
        "{action}: {}",
        trim_error_detail(error, 1024, "No error details were provided.")
    );
    crate::backend::irc::add_system_message(
        channel.trim().trim_start_matches('#'),
        &text,
        crate::backend::models::SystemMessageType::Error {
            message: text.clone(),
        },
    );
}

fn push_command_success_feedback(channel: &str, status: &str) {
    let status = status.trim();
    if status.is_empty() {
        return;
    }

    let channel = channel.trim().trim_start_matches('#');
    if channel.is_empty() {
        crate::backend::push_ui_action(crate::backend::UiAction::ShowSuccessToast {
            message: status.to_string(),
            detail: None,
        });
        return;
    }

    crate::backend::irc::add_system_message(
        channel,
        status,
        crate::backend::models::SystemMessageType::Info,
    );
}

/// Typed-command moderation feedback: which mod-log action a slash command
/// maps to, the label used for synthesized success text, and the generic
/// message shown instead of details while streamer mode hides mod actions.
struct ModerationCommandFeedback {
    action_type: Option<crate::backend::components::mod_log::ModActionType>,
    action_label: &'static str,
    hidden_message: &'static str,
}

fn moderation_command_feedback(command: &str) -> Option<ModerationCommandFeedback> {
    use crate::backend::components::mod_log::ModActionType;

    let (action_type, action_label, hidden_message) = match command {
        "timeout" | "to" => (
            Some(ModActionType::Timeout),
            "Timed out",
            "Timed out a user.",
        ),
        "ban" | "banid" => (Some(ModActionType::Ban), "Banned", "Banned a user."),
        "unban" | "untimeout" => (
            Some(ModActionType::Unban),
            "Removed ban or timeout for",
            "Removed a ban or timeout.",
        ),
        "delete" => (
            Some(ModActionType::Delete),
            "Deleted message",
            "Deleted a message.",
        ),
        "clear" => (
            Some(ModActionType::Clear),
            "Cleared chat in",
            "Cleared chat.",
        ),
        "mod" => (
            Some(ModActionType::Mod),
            "Added moderator",
            "Added a moderator.",
        ),
        "unmod" => (
            Some(ModActionType::Unmod),
            "Removed moderator",
            "Removed a moderator.",
        ),
        "vip" => (Some(ModActionType::Vip), "Added VIP", "Added a VIP."),
        "unvip" => (Some(ModActionType::Unvip), "Removed VIP", "Removed a VIP."),
        "warn" => (Some(ModActionType::Warning), "Warned", "Warned a user."),
        "block" | "ignore" => (None, "Blocked", "Blocked a user."),
        "unblock" | "unignore" => (None, "Unblocked", "Unblocked a user."),
        _ => return None,
    };
    Some(ModerationCommandFeedback {
        action_type,
        action_label,
        hidden_message,
    })
}

/// Extract the lowercased slash-command token from an expanded input line.
fn slash_command_token(message: &str) -> Option<String> {
    let first = message.split_whitespace().next()?;
    let command = first.strip_prefix('/')?;
    if command.is_empty() {
        return None;
    }
    Some(command.to_ascii_lowercase())
}

/// First typed argument of the command, stripped of @ and leading #.
fn first_command_arg(message: &str) -> Option<&str> {
    let mut parts = message.split_whitespace();
    parts.next()?;
    parts
        .next()
        .map(|arg| arg.trim_start_matches('@').trim_start_matches('#'))
        .filter(|arg| !arg.is_empty())
}

fn push_moderation_command_feedback(
    channel: &str,
    message: &str,
    status: Option<&str>,
    feedback: &ModerationCommandFeedback,
) {
    let channel = channel.trim().trim_start_matches('#');
    let arg_target = first_command_arg(message);
    let target = arg_target.unwrap_or(channel);

    let visible = match status.map(str::trim).filter(|status| !status.is_empty()) {
        Some(status) => status.to_string(),
        None => match arg_target {
            Some(target) => format!("{} {} in #{}", feedback.action_label, target, channel),
            None => format!("{} #{}", feedback.action_label, channel),
        },
    };

    if let Some(action_type) = feedback.action_type {
        crate::backend::components::mod_log::record_local_mod_action(
            action_type,
            target.to_string(),
            channel.to_string(),
            visible.clone(),
        );
    }
    super::moderation::push_moderation_success_toast(visible, feedback.hidden_message);
}

/// Trims `error`, caps it at `max_chars` characters (appending `...` when cut),
/// and returns `empty_fallback` when nothing remains.
pub(crate) fn trim_error_detail(error: &str, max_chars: usize, empty_fallback: &str) -> String {
    let error = error.trim();
    let mut detail: String = error.chars().take(max_chars).collect();
    if error.chars().count() > max_chars {
        detail.push_str("...");
    }
    if detail.is_empty() {
        empty_fallback.to_string()
    } else {
        detail
    }
}

/// Send a message to a channel (async-friendly, spawns task)
/// Returns immediately without blocking the UI thread
pub fn send_message_async(channel: String, message: String) {
    send_message_from_draft_async(channel, message, None);
}

/// [`send_message_async`] for a composer draft that goes back to `origin` on rejection.
pub fn send_message_from_draft_async(
    channel: String,
    message: String,
    origin: Option<SendDraftOrigin>,
) {
    let mut completion = ComposerSendCompletion::new(&channel, "Message", origin);
    if let Err(error) = crate::bench::reject_live_outbound_user_action("chat message") {
        crate::log_irc!(warn, "{error}");
        completion.failed(&error.into());
        return;
    }
    let send_auth = crate::backend::auth::capture_authenticated_user_resource();
    RUNTIME.spawn(async move {
        let result = match send_auth {
            Some(auth) => crate::backend::irc::send_message_with_auth(&channel, &message, auth).await,
            None => Err("No authenticated Twitch account was ready when this message was submitted. Log in and try again.".into()),
        };
        if let Err(e) = result {
            crate::log_irc!(error, "Failed to send message to {}: {}", channel, e);
            completion.failed(&e);
        } else { completion.accepted(unconfirmed_action(&message)); }
    });
}

/// Send a message, trying command dispatch first
/// If input starts with /, route through command engine
/// Otherwise send as regular chat message
pub fn send_message_or_command_async(channel: String, message: String) {
    send_message_or_command_from_draft_async(channel, message, None);
}

/// Dispatch a composer submission with account-bound outcome feedback.
pub fn send_message_or_command_from_draft_async(
    channel: String,
    message: String,
    origin: Option<SendDraftOrigin>,
) {
    let mut completion = ComposerSendCompletion::new(&channel, "Command", origin);
    if let Err(error) = crate::bench::reject_live_outbound_user_action("chat submission") {
        crate::log_irc!(warn, "{error}");
        completion.failed(&error.into());
        return;
    }
    let moderation_auth = crate::backend::auth::capture_authenticated_user_resource();
    let send_auth = moderation_auth.clone();
    RUNTIME.spawn(async move {
        let message = crate::backend::commands::expand_custom_command(&channel, &message)
            .await
            .unwrap_or(message);
        let result = match crate::backend::commands::helix::execute_typed_moderation_command(
            &channel,
            &message,
            moderation_auth,
        )
        .await
        {
            Some(fenced) => match fenced.publication() {
                crate::backend::commands::helix::FencedModerationCommandPublication::Publish(result) => {
                    result
                }
                crate::backend::commands::helix::FencedModerationCommandPublication::Suppress {
                    remote_request_reported_success,
                } => {
                    if remote_request_reported_success {
                        crate::log_auth!(
                            debug,
                            "Suppressed stale typed moderation success; Twitch may have applied the request for the previous account"
                        );
                    } else {
                        crate::log_auth!(
                            debug,
                            "Suppressed stale typed moderation result; it did not report success, but Twitch may have applied the prior-account request"
                        );
                    }
                    return;
                }
            },
            None => crate::backend::commands::execute_command(&channel, &message).await,
        };
        match result {
            crate::backend::commands::CommandResult::NotACommand => {
                if crate::backend::whispers::is_whispers_channel(&channel) {
                    completion.failed(&"Use /w <user> <message> or /r <message> in the Whispers split.".into());
                    return;
                }
                let result = match send_auth {
                    Some(auth) => crate::backend::irc::send_message_with_auth(&channel, &message, auth).await,
                    None => Err("No authenticated Twitch account was ready when this message was submitted. Log in and try again.".into()),
                };
                if let Err(e) = result {
                    crate::log_irc!(error, "Failed to send message to {}: {}", channel, e);
                    completion.failed(&e);
                } else { completion.accepted(unconfirmed_action(&message)); }
            }
            crate::backend::commands::CommandResult::Success(status) => {
                if matches!(slash_command_token(&message).as_deref(), Some("unban" | "untimeout"))
                    && let Some(target) = message.split_whitespace().nth(1)
                    && let Some(target_id) = target.strip_prefix("id:").map(str::to_string)
                        .or_else(|| crate::backend::twitch_api::get_cached_channel_id(target.trim_start_matches('@')))
                    && let Some(auth) = send_auth.as_ref()
                {
                    auth.commit_if_current(|| crate::backend::irc::room_state::clear_send_restriction_for_user(&channel, &target_id));
                }
                completion.accepted(false);
                if let Some(msg) = status.as_deref() {
                    crate::log_irc!(info, "Command success: {}", msg);
                }
                let moderation_feedback = slash_command_token(&message)
                    .as_deref()
                    .and_then(moderation_command_feedback);
                if let Some(feedback) = moderation_feedback {
                    push_moderation_command_feedback(
                        &channel,
                        &message,
                        status.as_deref(),
                        &feedback,
                    );
                } else if let Some(msg) = status {
                    push_command_success_feedback(&channel, &msg);
                }
            }
            crate::backend::commands::CommandResult::Error(err) => {
                crate::log_irc!(error, "Command error: {}", err);
                completion.failed(&crate::backend::irc::SendError { message: err, delivery_unknown: false });
            }
            crate::backend::commands::CommandResult::DeliveryUnknown(err) => {
                crate::log_irc!(error, "Command outcome unknown: {}", err);
                completion.failed(&crate::backend::irc::SendError { message: err, delivery_unknown: true });
            }
        }
    });
}

/// Open the chatter list for a channel from UI affordances outside chat input.
pub fn open_chatter_list_async(channel: String) {
    let channel = channel.trim_start_matches('#').trim().to_lowercase();
    if channel.is_empty() {
        return;
    }

    crate::backend::push_ui_action(crate::backend::UiAction::OpenChatterListLoading {
        channel: channel.clone(),
    });
    crate::managers::frame_rate_controller::record_activity();

    RUNTIME.spawn(async move {
        match crate::backend::commands::helix::cmd_chatters(&channel).await {
            crate::backend::commands::CommandResult::Success(status) => {
                if let Some(message) = status {
                    crate::log_irc!(info, "Chatter list loaded: {}", message);
                }
            }
            crate::backend::commands::CommandResult::Error(error)
            | crate::backend::commands::CommandResult::DeliveryUnknown(error) => {
                crate::log_irc!(
                    warn,
                    "Failed to load chatter list for #{}: {}",
                    channel,
                    error
                );
                publish_command_error(&channel, "Failed to load chatter list", &error);
            }
            crate::backend::commands::CommandResult::NotACommand => {}
        }
        crate::managers::frame_rate_controller::record_activity();
    });
}

/// Send a reply to a message (async-friendly, spawns task)
/// Returns immediately without blocking the UI thread
pub fn send_reply_async(
    channel: String,
    message: String,
    reply_to_msg_id: String,
    reply_to_user: String,
) {
    send_reply_from_draft_async(channel, message, reply_to_msg_id, reply_to_user, None);
}

/// [`send_reply_async`] for a composer draft that goes back to `origin` on rejection.
pub fn send_reply_from_draft_async(
    channel: String,
    message: String,
    reply_to_msg_id: String,
    reply_to_user: String,
    origin: Option<SendDraftOrigin>,
) {
    let mut completion = ComposerSendCompletion::new(&channel, "Reply", origin);
    if let Err(error) = crate::bench::reject_live_outbound_user_action("chat reply") {
        crate::log_irc!(warn, "{error}");
        completion.failed(&error.into());
        return;
    }
    let send_auth = crate::backend::auth::capture_authenticated_user_resource();
    RUNTIME.spawn(async move {
        let result = match send_auth {
            Some(auth) => crate::backend::irc::send_reply_with_auth(
                &channel, &message, &reply_to_msg_id, &reply_to_user, auth
            ).await,
            None => Err("No authenticated Twitch account was ready when this reply was submitted. Log in and try again.".into()),
        };
        if let Err(e) = result {
            crate::log_irc!(error, "Failed to send reply to {}: {}", channel, e);
            completion.failed(&e);
        } else { completion.accepted(unconfirmed_action(&message)); }
    });
}

/// Upload a pasted RGBA clipboard image using the configured image uploader.
///
/// Returns true when the paste was handled as an image upload. A false return
/// means the caller should fall back to normal text paste behavior.
pub fn upload_pasted_image_async(
    channel: String,
    split_id: crate::state::SplitId,
    width: usize,
    height: usize,
    rgba: Vec<u8>,
) -> bool {
    let config = crate::backend::settings::get_image_uploader_config();
    if !config.enabled || !config.paste_upload_enabled {
        return false;
    }

    let channel = channel.trim_start_matches('#').to_lowercase();
    if channel.is_empty() {
        return false;
    }
    if let Err(error) = crate::bench::reject_live_outbound_user_action("image paste upload") {
        crate::log_irc!(warn, "{error}");
        add_upload_error(&channel, &error);
        return true;
    }

    let job = crate::backend::upload_coordinator::UploadJob {
        channel: channel.clone(),
        split_id,
        config: Arc::new(config),
        source: crate::backend::upload_coordinator::UploadSource::ClipboardRgba {
            width,
            height,
            rgba,
        },
    };
    match crate::backend::upload_coordinator::enqueue_batch(vec![job]) {
        Ok(worker_token) => start_upload_worker(worker_token),
        Err(reason) => {
            reject_upload_queue(&channel, reason);
        }
    }

    true
}

/// Upload dropped or pasted local image files using the configured image
/// uploader.
///
/// Returns the number of paths accepted for background processing. A zero
/// return means the files were not handled and callers can ignore them.
pub fn upload_image_files_async(
    channel: String,
    split_id: crate::state::SplitId,
    paths: Vec<PathBuf>,
    source: crate::backend::image_uploader::ImageFileSource,
) -> usize {
    let config = crate::backend::settings::get_image_uploader_config();
    if !source.upload_enabled(&config) || paths.is_empty() {
        return 0;
    }

    let channel = channel.trim_start_matches('#').to_lowercase();
    if channel.is_empty() {
        return 0;
    }
    if let Err(error) = crate::bench::reject_live_outbound_user_action("image file upload") {
        crate::log_irc!(warn, "{error}");
        add_upload_error(&channel, &error);
        return 0;
    }

    let count = paths.len();
    let config = Arc::new(config);
    let jobs = paths
        .into_iter()
        .map(|path| crate::backend::upload_coordinator::UploadJob {
            channel: channel.clone(),
            split_id,
            config: config.clone(),
            source: crate::backend::upload_coordinator::UploadSource::DroppedFile { path },
        })
        .collect();
    match crate::backend::upload_coordinator::enqueue_batch(jobs) {
        Ok(worker_token) => {
            start_upload_worker(worker_token);
            count
        }
        Err(reason) => {
            reject_upload_queue(&channel, reason);
            0
        }
    }
}

fn start_upload_worker(worker_token: Option<u64>) {
    if let Some(worker_token) = worker_token {
        RUNTIME.spawn(run_upload_worker(worker_token));
    }
}

fn reject_upload_queue(channel: &str, reason: crate::backend::upload_coordinator::AdmissionError) {
    crate::log_irc!(
        warn,
        channel,
        ?reason,
        max_items = crate::backend::upload_coordinator::MAX_UPLOAD_ITEMS,
        max_retained_bytes = crate::backend::upload_coordinator::MAX_RETAINED_UPLOAD_BYTES,
        "Image upload rejected by bounded coordinator"
    );
    add_upload_error(
        channel,
        &format!(
            "Image upload not queued: the {}-item / {} upload queue is full.",
            crate::backend::upload_coordinator::MAX_UPLOAD_ITEMS,
            crate::backend::image_uploader::format_upload_size(
                crate::backend::upload_coordinator::MAX_RETAINED_UPLOAD_BYTES as u64,
            )
        ),
    );
}

async fn run_upload_worker(worker_token: u64) {
    let _worker_lease = crate::backend::upload_coordinator::worker_lease(worker_token);
    while let Some(reservation) = crate::backend::upload_coordinator::take_next(worker_token) {
        let panic_channel = reservation.channel().to_string();
        let result = AssertUnwindSafe(process_reserved_upload(reservation))
            .catch_unwind()
            .await;
        if result.is_err() {
            crate::log_irc!(
                error,
                channel = %panic_channel,
                "Image upload job panicked; continuing bounded queue"
            );
            add_upload_error(
                &panic_channel,
                "Image upload failed unexpectedly; queued uploads will continue.",
            );
        }
    }
}

async fn process_reserved_upload(
    mut reservation: crate::backend::upload_coordinator::UploadReservation,
) {
    let Some(job) = reservation.take_job() else {
        return;
    };
    let crate::backend::upload_coordinator::UploadJob {
        channel,
        split_id,
        config,
        source,
    } = job;

    match source {
        crate::backend::upload_coordinator::UploadSource::ClipboardRgba {
            width,
            height,
            rgba,
        } => {
            upload_clipboard_image(
                &channel,
                split_id,
                &config,
                width,
                height,
                rgba,
                &mut reservation,
            )
            .await;
        }
        crate::backend::upload_coordinator::UploadSource::DroppedFile { path } => {
            upload_image_file_for_channel(&channel, split_id, &config, path, &mut reservation)
                .await;
        }
    }
}

async fn upload_clipboard_image(
    channel: &str,
    split_id: crate::state::SplitId,
    config: &crate::backend::image_uploader::ImageUploaderConfig,
    width: usize,
    height: usize,
    rgba: Vec<u8>,
    reservation: &mut crate::backend::upload_coordinator::UploadReservation,
) {
    let source_size = rgba.len();
    let source_capacity = rgba.capacity();
    crate::backend::irc::add_system_message(
        channel,
        &format!(
            "Uploading pasted image ({}x{}, {})...",
            width,
            height,
            crate::backend::image_uploader::format_upload_size(source_size as u64)
        ),
        crate::backend::models::SystemMessageType::UploadStart,
    );

    let encoded = RUNTIME
        .spawn_blocking(move || {
            crate::backend::image_uploader::encode_rgba_png(width, height, rgba)
        })
        .await;
    let png = match encoded {
        Ok(Ok(png)) => png,
        Ok(Err(err)) => {
            add_upload_error(channel, &format!("Image upload failed: {err}"));
            return;
        }
        Err(err) => {
            add_upload_error(
                channel,
                &format!("Image upload failed while encoding clipboard data: {err}"),
            );
            return;
        }
    };

    if png.capacity() > source_capacity
        && !reservation.reserve_additional_bytes(png.capacity() - source_capacity)
    {
        add_upload_error(
            channel,
            "Image upload stopped: the encoded image exceeds the upload memory budget.",
        );
        return;
    }

    crate::backend::upload_coordinator::wait_for_provider_request_slot().await;
    let filename = format!("echo-paste-{}.png", chrono::Utc::now().timestamp_millis());
    match crate::backend::image_uploader::upload_image_bytes(config, &filename, "image/png", png)
        .await
    {
        Ok(result) => add_upload_success(channel, split_id, config, &filename, "image/png", result),
        Err(err) => add_upload_error(channel, &format!("Image upload failed: {err}")),
    }
}

async fn upload_image_file_for_channel(
    channel: &str,
    split_id: crate::state::SplitId,
    config: &crate::backend::image_uploader::ImageUploaderConfig,
    path: PathBuf,
    reservation: &mut crate::backend::upload_coordinator::UploadReservation,
) {
    let file_label = path
        .file_name()
        .and_then(|name| name.to_str())
        .map(str::to_string)
        .unwrap_or_else(|| path.display().to_string());

    let Some(mime_type) = crate::backend::image_uploader::mime_type_for_image_path(&path) else {
        add_upload_error(
            channel,
            &format!("Unsupported upload file type: {file_label}"),
        );
        return;
    };

    let metadata = match tokio::fs::metadata(&path).await {
        Ok(metadata) => metadata,
        Err(err) => {
            add_upload_error(
                channel,
                &format!("Could not read upload file metadata for {file_label}: {err}"),
            );
            return;
        }
    };
    if !metadata.is_file() {
        add_upload_error(channel, &format!("Upload path is not a file: {file_label}"));
        return;
    }
    if metadata.len() == 0 {
        add_upload_error(
            channel,
            &format!("Cannot upload an empty file: {file_label}"),
        );
        return;
    }
    if metadata.len() > config.max_upload_bytes {
        add_upload_error(
            channel,
            &format!(
                "{} is larger than the configured {} byte upload limit",
                file_label, config.max_upload_bytes
            ),
        );
        return;
    }

    let Ok(file_size) = usize::try_from(metadata.len()) else {
        add_upload_error(
            channel,
            &format!("Upload file is too large for this platform: {file_label}"),
        );
        return;
    };
    if !reservation.reserve_additional_bytes(file_size) {
        add_upload_error(
            channel,
            "Image upload not started: the upload memory budget is full.",
        );
        return;
    }

    crate::backend::irc::add_system_message(
        channel,
        &format!(
            "Uploading {} ({})...",
            file_label,
            crate::backend::image_uploader::format_upload_size(metadata.len())
        ),
        crate::backend::models::SystemMessageType::UploadStart,
    );

    let bytes = match tokio::fs::read(&path).await {
        Ok(bytes) => bytes,
        Err(err) => {
            add_upload_error(
                channel,
                &format!("Could not read upload file {file_label}: {err}"),
            );
            return;
        }
    };
    if bytes.capacity() > file_size
        && !reservation.reserve_additional_bytes(bytes.capacity() - file_size)
    {
        add_upload_error(
            channel,
            "Image upload stopped: the file exceeds the upload memory budget.",
        );
        return;
    }

    crate::backend::upload_coordinator::wait_for_provider_request_slot().await;
    match crate::backend::image_uploader::upload_image_bytes(config, &file_label, mime_type, bytes)
        .await
    {
        Ok(result) => add_upload_success(channel, split_id, config, &file_label, mime_type, result),
        Err(err) => add_upload_error(channel, &format!("Image upload failed: {err}")),
    }
}

fn add_upload_success(
    channel: &str,
    split_id: crate::state::SplitId,
    config: &crate::backend::image_uploader::ImageUploaderConfig,
    file_name: &str,
    mime_type: &str,
    result: crate::backend::image_uploader::UploadResult,
) {
    let copy_suffix = if config.copy_link_after_upload {
        match crate::utils::set_clipboard_text(&result.image_link) {
            Ok(()) => " and copied link".to_string(),
            Err(err) => {
                crate::log_ui!(warn, "Failed to copy uploaded image link: {err}");
                format!(" (copy failed: {err})")
            }
        }
    } else {
        String::new()
    };
    // Anyone holding the deletion link can remove the image, so it stays out
    // of chat while streamer mode is active; upload history still keeps it.
    let hide_deletion_link = crate::backend::streamer_mode::is_streamer_mode_active();
    let delete_suffix = match result.deletion_link.as_ref() {
        Some(_) if hide_deletion_link => " (delete link saved in upload history)".to_string(),
        Some(delete_url) => format!(" Delete: {delete_url}"),
        None => String::new(),
    };
    let shown_deletion_link = if hide_deletion_link {
        None
    } else {
        result.deletion_link.clone()
    };
    crate::backend::upload_history::record_upload(
        result.image_link.clone(),
        result.deletion_link.clone(),
        file_name,
        mime_type,
        channel,
    );
    crate::backend::push_ui_action(crate::backend::UiAction::InsertUploadedLink {
        split_id,
        channel: channel.to_string(),
        url: result.image_link.clone(),
    });
    crate::backend::irc::add_system_message(
        channel,
        &format!(
            "Uploaded image{}: {}{}",
            copy_suffix, result.image_link, delete_suffix
        ),
        crate::backend::models::SystemMessageType::UploadSuccess {
            url: result.image_link,
            delete_url: shown_deletion_link,
        },
    );
}

fn add_upload_error(channel: &str, message: &str) {
    crate::backend::irc::add_system_message(
        channel,
        message,
        crate::backend::models::SystemMessageType::UploadError {
            error: message.to_string(),
        },
    );
}

/// Local buffered context for a user in one channel.
#[derive(Clone, Debug, Default)]
pub struct UserMessageContext {
    pub recent_messages: Vec<ChatMessage>,
    pub buffered_message_count: usize,
    pub first_seen_timestamp: Option<i64>,
    pub last_seen_timestamp: Option<i64>,
    pub latest_display_name: Option<String>,
    pub latest_color: Option<String>,
    pub latest_badges: Vec<String>,
    pub latest_user_id: Option<String>,
}

/// Get messages for a channel
/// Clones only the requested visible tail under the short visibility gate.
pub fn get_messages(channel: &str, limit: u32) -> Vec<ChatMessage> {
    let result = crate::backend::irc::visible_channel_messages_tail(channel, limit as usize);

    for msg in &result {
        if let Some(ref color) = msg.color {
            crate::backend::irc::cache_user_color(&msg.sender, color);
        }
    }

    result
}

/// Get recent messages and local session metadata for a user.
///
/// This intentionally stays local and non-blocking for user cards. Followage,
/// subage, and other live account facts can be layered on later without making
/// the card wait on Helix.
pub fn get_user_message_context(channel: &str, username: &str, limit: u32) -> UserMessageContext {
    let limit = limit as usize;
    let username_lower = username.to_lowercase();
    let mut context = UserMessageContext::default();
    let mut recent_messages = VecDeque::with_capacity(limit.max(1));

    {
        let visibility_commit = crate::backend::blocked_users::visibility_commit_guard();
        if crate::backend::blocked_users::resident_visibility_revoked() {
            return context;
        }
        let channel = channel.to_lowercase();
        let pending_ids = crate::backend::blocked_users::pending_channel_row_ids_commit_held(
            &visibility_commit,
            &channel,
        );
        let Some(buffer) = crate::backend::irc::CHAT_MESSAGES.get(&channel) else {
            return context;
        };

        for msg in buffer.iter() {
            if pending_ids.contains(msg.id.as_ref())
                || !msg.sender.as_ref().eq_ignore_ascii_case(&username_lower)
            {
                continue;
            }

            context.buffered_message_count += 1;
            if context.first_seen_timestamp.is_none() {
                context.first_seen_timestamp = Some(msg.timestamp);
            }
            context.last_seen_timestamp = Some(msg.timestamp);

            if !msg.display_name.is_empty() {
                context.latest_display_name = Some(msg.display_name.to_string());
            }
            if let Some(color) = msg.color.as_ref() {
                context.latest_color = Some(color.to_string());
            }
            if !msg.badges.is_empty() {
                context.latest_badges = msg.badges.iter().map(ToString::to_string).collect();
            }
            if let Some(user_id) = msg.user_id.as_ref() {
                context.latest_user_id = Some(user_id.to_string());
            }

            if limit > 0 {
                if recent_messages.len() == limit {
                    recent_messages.pop_front();
                }
                recent_messages.push_back(msg.clone());
            }
        }
        drop(buffer);
        drop(visibility_commit);
    }

    context.recent_messages = recent_messages.into_iter().collect();
    context
}

/// Fingerprint of everything `get_user_message_context` would expose for a user.
///
/// Mirrors that function's visibility filter but hashes instead of cloning, so a
/// caller polling for updates can skip the rebuild while nothing it renders has
/// changed. Returns 0 for the cases that produce an empty context (revoked
/// visibility, no buffer); every other result is non-zero.
pub fn get_user_message_context_fingerprint(channel: &str, username: &str) -> u64 {
    use std::hash::{Hash, Hasher};

    let username_lower = username.to_lowercase();

    let visibility_commit = crate::backend::blocked_users::visibility_commit_guard();
    if crate::backend::blocked_users::resident_visibility_revoked() {
        return 0;
    }
    let channel = channel.to_lowercase();
    let pending_ids = crate::backend::blocked_users::pending_channel_row_ids_commit_held(
        &visibility_commit,
        &channel,
    );
    let Some(buffer) = crate::backend::irc::CHAT_MESSAGES.get(&channel) else {
        return 0;
    };

    let mut hasher = rustc_hash::FxHasher::default();
    let mut matched: usize = 0;
    for msg in buffer.iter() {
        if pending_ids.contains(msg.id.as_ref())
            || !msg.sender.as_ref().eq_ignore_ascii_case(&username_lower)
        {
            continue;
        }

        matched += 1;
        msg.id.hash(&mut hasher);
        msg.timestamp.hash(&mut hasher);
        msg.structure_hash.hash(&mut hasher);
        msg.display_name.hash(&mut hasher);
        msg.color.hash(&mut hasher);
        msg.badges.hash(&mut hasher);
        msg.user_id.hash(&mut hasher);
    }
    drop(buffer);
    drop(visibility_commit);

    matched.hash(&mut hasher);
    hasher.finish() | 1
}

/// Get recent chatters for autocomplete
pub fn get_recent_chatters(
    channel: &str,
    limit: usize,
) -> Vec<crate::backend::irc::ObservedChatter> {
    crate::backend::irc::get_recent_chatters(channel, limit)
}

/// Trim message buffers for inactive channels to reduce memory usage.
/// Active channels keep full capacity. Inactive channels keep a bounded history
/// large enough for smooth tab switching while reducing background RAM.
///
/// Call this periodically (e.g., every 30 seconds) to maintain memory efficiency.
pub fn trim_inactive_channels(active_channels: &[String]) {
    use std::collections::HashSet;

    let active_set: HashSet<String> = active_channels.iter().map(|c| c.to_lowercase()).collect();

    let full_capacity = crate::state::message_retention::active_channel_capacity();
    let inactive_capacity = crate::state::message_retention::inactive_channel_capacity();

    let mut active_restored = 0;
    let mut inactive_trimmed = 0;
    let mut rows_evicted = false;

    for mut entry in crate::backend::irc::CHAT_MESSAGES.iter_mut() {
        let channel = entry.key().clone();
        let buffer = entry.value_mut();

        if active_set.contains(&channel) {
            if buffer.capacity() != full_capacity {
                rows_evicted |= buffer.set_capacity(full_capacity);
                active_restored += 1;
            }
        } else if buffer.capacity() != inactive_capacity || buffer.len() > inactive_capacity {
            let contents_changed = buffer.set_capacity(inactive_capacity);
            if contents_changed {
                rows_evicted = true;
                inactive_trimmed += 1;
            }
        }
    }

    if rows_evicted {
        crate::backend::irc::bump_chat_version();
    }

    if active_restored > 0 || inactive_trimmed > 0 {
        crate::log_irc!(
            debug,
            active_restored,
            inactive_trimmed,
            full_capacity,
            inactive_capacity,
            "Rebalanced backend message buffers"
        );
    }
}

static STREAM_LIVE_STATUS: LazyLock<DashMap<String, bool>> = LazyLock::new(DashMap::new);
static LAST_STREAM_INFO: LazyLock<DashMap<String, StreamInfo>> = LazyLock::new(DashMap::new);
static PENDING_STREAM_INFO: LazyLock<DashMap<String, Option<StreamInfo>>> =
    LazyLock::new(DashMap::new);
static STREAM_INFO_DIRTY: AtomicBool = AtomicBool::new(false);
static STREAM_FETCH_IN_FLIGHT: AtomicBool = AtomicBool::new(false);

struct StreamFetchInFlightGuard;

impl Drop for StreamFetchInFlightGuard {
    fn drop(&mut self) {
        STREAM_FETCH_IN_FLIGHT.store(false, Ordering::Release);
    }
}

fn normalize_stream_status_channel(channel: &str) -> String {
    channel.trim().trim_start_matches('#').to_lowercase()
}

pub fn set_cached_channel_live_status(channel: &str, is_live: bool) {
    let channel_key = normalize_stream_status_channel(channel);
    if !channel_key.is_empty() {
        STREAM_LIVE_STATUS.insert(channel_key, is_live);
    }
}

pub fn get_cached_channel_live_status(channel: &str) -> bool {
    let channel_key = normalize_stream_status_channel(channel);
    if channel_key.is_empty() {
        false
    } else {
        STREAM_LIVE_STATUS
            .get(&channel_key)
            .map(|status| *status)
            .unwrap_or(false)
    }
}

/// Whole minutes a live stream has been running, or 0 while offline. Split
/// headers print uptime at minute resolution, so this is their refresh key.
pub fn live_uptime_minutes(info: Option<&StreamInfo>) -> i64 {
    info.filter(|info| info.is_live && info.started_at > 0)
        .map_or(0, |info| {
            ((crate::utils::unix_now_secs() as i64 - info.started_at) / 60).max(0)
        })
}

/// Change key for stream info; it hashes the fields `StreamInfo`'s `Eq` compares.
pub fn stream_info_gate_hash(info: Option<&StreamInfo>) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = ahash::AHasher::default();
    info.hash(&mut hasher);
    hasher.finish()
}

async fn fetch_stream_info(channel: &str) -> Option<StreamInfo> {
    match crate::backend::twitch_api::get_stream_info(channel).await {
        Ok(Some(result)) => {
            set_cached_channel_live_status(channel, result.is_live);
            let info = StreamInfo {
                title: result.title,
                game_name: result.game_name,
                viewer_count: result.viewer_count,
                started_at: result.started_at,
                is_live: result.is_live,
            };
            LAST_STREAM_INFO.insert(normalize_stream_status_channel(channel), info.clone());
            Some(info)
        }
        Ok(None) => {
            set_cached_channel_live_status(channel, false);
            let info = StreamInfo {
                title: String::new(),
                game_name: None,
                viewer_count: 0,
                started_at: 0,
                is_live: false,
            };
            LAST_STREAM_INFO.insert(normalize_stream_status_channel(channel), info.clone());
            Some(info)
        }
        // A logged-out user of a build without the proxy hits this on every
        // poll; that is expected, so it stays out of the WARN log.
        Err(e) if e.is_proxy_not_configured() => {
            crate::log_net!(debug, "Skipped stream info for {}: {}", channel, e);
            None
        }
        Err(e) => {
            crate::log_net!(warn, "Failed to get stream info for {}: {}", channel, e);
            None
        }
    }
}

pub fn stage_eventsub_stream_info(channel: &str, is_live: bool, started_at: Option<i64>) -> u64 {
    let channel_key = normalize_stream_status_channel(channel);
    let previous_info = LAST_STREAM_INFO.get(&channel_key).map(|info| info.clone());
    let previous_hash = stream_info_gate_hash(previous_info.as_ref());
    let mut info = previous_info.unwrap_or(StreamInfo {
        title: String::new(),
        game_name: None,
        viewer_count: 0,
        started_at: 0,
        is_live,
    });

    info.is_live = is_live;
    if is_live {
        if let Some(started_at) = started_at.filter(|started_at| *started_at > 0) {
            info.started_at = started_at;
        }
    } else {
        info.viewer_count = 0;
    }

    let info_hash = stream_info_gate_hash(Some(&info));
    if info_hash == previous_hash {
        return info_hash;
    }

    LAST_STREAM_INFO.insert(channel_key.clone(), info.clone());
    PENDING_STREAM_INFO.insert(channel_key, Some(info));
    STREAM_INFO_DIRTY.store(true, Ordering::Release);
    crate::managers::frame_rate_controller::wakeup();
    info_hash
}

pub fn queue_stream_info_refresh(
    mut channels: Vec<String>,
    last_hashes: Vec<(String, u64)>,
) -> bool {
    if crate::bench::ui_fixture::disposable_workspace_enabled() {
        return false;
    }

    channels.sort_unstable();
    channels.dedup();
    if channels.is_empty() {
        return false;
    }
    if STREAM_FETCH_IN_FLIGHT
        .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
        .is_err()
    {
        crate::log_net!(debug, "stream refresh skipped: fetch already in flight");
        return false;
    }

    let channel_count = channels.len();
    let render_mode = crate::managers::frame_rate_controller::current_mode_cached();
    crate::log_net!(
        debug,
        "stream refresh: {} channels queued (mode={:?})",
        channel_count,
        render_mode
    );
    let last_hashes: HashMap<String, u64> = last_hashes.into_iter().collect();

    RUNTIME.spawn(async move {
        let _in_flight = StreamFetchInFlightGuard;
        let mut changed = 0usize;

        for channel in channels {
            let info = fetch_stream_info(&channel).await;
            let previous_hash = last_hashes.get(&channel).copied();
            let should_stage = match info.as_ref() {
                Some(info) => previous_hash != Some(stream_info_gate_hash(Some(info))),
                None => previous_hash.is_none(),
            };

            if should_stage {
                PENDING_STREAM_INFO.insert(channel, info);
                changed += 1;
            }
        }

        crate::log_net!(
            debug,
            "stream refresh: {} fetched, {} changed",
            channel_count,
            changed
        );
        if changed > 0 {
            STREAM_INFO_DIRTY.store(true, Ordering::Release);
            crate::managers::frame_rate_controller::wakeup();
        }
    });

    true
}

pub fn drain_pending_stream_info() -> Option<Vec<(String, Option<StreamInfo>)>> {
    if !STREAM_INFO_DIRTY.swap(false, Ordering::AcqRel) {
        return None;
    }

    let mut pending = Vec::with_capacity(PENDING_STREAM_INFO.len());
    PENDING_STREAM_INFO.retain(|channel, info| {
        pending.push((channel.clone(), info.clone()));
        false
    });
    (!pending.is_empty()).then_some(pending)
}

/// Get stream info for a channel
pub fn get_stream_info(channel: &str) -> Option<StreamInfo> {
    let channel_owned = channel.to_string();
    let channel_for_log = channel_owned.clone();
    block_on_safe(async move { fetch_stream_info(&channel_owned).await }).unwrap_or_else(|e| {
        crate::log_net!(
            error,
            "Failed to get stream info for {}: {}",
            channel_for_log,
            e
        );
        None
    })
}

/// `get_stream_info` for async callers, which must not block a runtime worker.
pub async fn get_stream_info_async(channel: &str) -> Option<StreamInfo> {
    fetch_stream_info(channel).await
}

/// Get channel ID from login name (checks cache first for O(1) lookup)
/// Non-blocking version for use on UI thread
pub fn get_channel_id_cached(channel: &str) -> Option<String> {
    crate::backend::twitch_api::get_cached_channel_id(channel)
}

/// Resolve a channel ID without blocking or creating a nested runtime.
pub(crate) async fn get_channel_id_async(channel: &str) -> Option<String> {
    match crate::backend::twitch_api::get_user_id_cached(channel).await {
        Ok(id) => Some(id),
        Err(error) => {
            crate::backend::twitch_api::log_lookup_failure("channel ID", channel, &error);
            None
        }
    }
}

/// Get channel ID from login name (checks cache first for O(1) lookup)
pub fn get_channel_id(channel: &str) -> Option<String> {
    if let Some(id) = get_channel_id_cached(channel) {
        return Some(id);
    }

    let channel = channel.to_string();
    let channel_for_log = channel.clone();
    block_on_safe(async move { get_channel_id_async(&channel).await }).unwrap_or_else(|error| {
        crate::log_net!(
            error,
            "Failed to resolve channel ID for {}: {}",
            channel_for_log,
            error
        );
        None
    })
}

/// Get whisper threads
pub fn get_whisper_threads() -> Vec<super::WhisperThread> {
    crate::backend::whispers::get_threads()
}

/// Mark a whisper thread as read.
pub fn mark_whisper_thread_read(username: &str) -> bool {
    crate::backend::whispers::mark_read(username)
}

/// Mark every whisper thread as read.
pub fn mark_all_whisper_threads_read() -> bool {
    crate::backend::whispers::mark_all_read()
}

/// Remove all retained Activity whisper summaries.
pub fn clear_all_whisper_threads() -> bool {
    crate::backend::whispers::clear_threads()
}

/// Get total unread whisper count.
pub fn get_unread_whisper_count() -> u32 {
    crate::backend::whispers::unread_count()
}

pub use crate::backend::commands::helix::{CommandAuthBlock, WhisperComposeBlock};

/// Return the current reason whisper compose/send should be treated as unavailable.
pub fn get_whisper_compose_block_reason() -> Option<WhisperComposeBlock> {
    crate::backend::commands::helix::whisper_compose_block_reason()
}

/// Get global mentions from O(1) ring buffer
/// Returns (channel, message) tuples sorted by timestamp descending (most recent first)
pub fn get_global_mentions(limit: u32) -> Vec<(String, ChatMessage)> {
    if crate::backend::blocked_users::resident_visibility_revoked() {
        return Vec::new();
    }
    let mentions = crate::backend::irc::GLOBAL_MENTIONS.read();
    mentions
        .iter()
        .rev()
        .take(limit as usize)
        .map(|(ch, msg)| (ch.clone(), msg.clone()))
        .collect()
}

/// Get total mentions currently retained for the sidebar.
pub fn get_global_mention_count() -> usize {
    if crate::backend::blocked_users::resident_visibility_revoked() {
        return 0;
    }
    crate::backend::irc::GLOBAL_MENTIONS.read().len()
}

/// Get the number of retained mentions that have not entered a visible canvas viewport.
pub fn get_unread_global_mention_count() -> usize {
    if crate::backend::blocked_users::resident_visibility_revoked() {
        return 0;
    }
    crate::backend::irc::unread_global_mention_count()
}

/// Clear mention attention while preserving retained Activity history.
pub fn mark_all_global_mentions_read() -> bool {
    let changed = crate::backend::irc::clear_global_mention_unread();
    if changed {
        crate::state::ui_invalidation::request(
            crate::state::ui_invalidation::UiInvalidation::TopBarState,
        );
    }
    changed
}

/// Clear attention for mention message IDs actually painted in visible chat viewports.
///
/// Takes borrowed ids: this runs on the UI poll path, so the owned lowercased key
/// set is materialized only when there is unread attention that could clear.
pub fn mark_visible_global_mentions_read(visible: &[(&str, &str)]) -> bool {
    if visible.is_empty() || crate::backend::irc::unread_global_mention_count() == 0 {
        return false;
    }
    let visible: HashSet<_> = visible
        .iter()
        .map(|(channel, message_id)| {
            (
                channel.trim_start_matches('#').to_lowercase(),
                (*message_id).to_string(),
            )
        })
        .collect();
    let changed = crate::backend::irc::mark_visible_global_mentions_read(&visible);
    if changed {
        crate::state::ui_invalidation::request(
            crate::state::ui_invalidation::UiInvalidation::TopBarState,
        );
    }
    changed
}

/// Clear the retained Activity Mentions history.
pub fn clear_global_mentions() {
    crate::backend::irc::GLOBAL_MENTIONS.write().clear();
    crate::backend::irc::clear_global_mention_unread();
    crate::state::ui_invalidation::request(
        crate::state::ui_invalidation::UiInvalidation::TopBarState,
    );
}

/// Remove one retained entry from Activity Mentions history.
pub fn remove_global_mention(channel: &str, message_id: &str) -> bool {
    let channel = channel.trim_start_matches('#').to_lowercase();
    let mut mentions = crate::backend::irc::GLOBAL_MENTIONS.write();
    let before = mentions.len();
    mentions.retain(|(mention_channel, msg)| {
        mention_channel.trim_start_matches('#').to_lowercase() != channel
            || msg.id.as_ref() != message_id
    });
    let changed = mentions.len() != before;
    if changed {
        crate::backend::irc::remove_global_mention_unread(&channel, message_id);
        crate::state::ui_invalidation::request(
            crate::state::ui_invalidation::UiInvalidation::TopBarState,
        );
    }
    changed
}

/// Clear every Activity source's unread state while preserving history.
pub fn mark_all_activity_read() -> bool {
    let mentions = mark_all_global_mentions_read();
    let whispers = mark_all_whisper_threads_read();
    let events = crate::backend::components::event_notification::mark_all_recent_events_read();
    mentions || whispers || events
}

/// Remove all retained Activity sources after an explicit user action.
pub fn clear_all_activity() -> bool {
    let had_mentions = get_global_mention_count() > 0;
    clear_global_mentions();
    let whispers = clear_all_whisper_threads();
    let had_events = !crate::backend::components::event_notification::recent_events(1).is_empty();
    crate::backend::components::event_notification::clear_recent_events();
    had_mentions || whispers || had_events
}
