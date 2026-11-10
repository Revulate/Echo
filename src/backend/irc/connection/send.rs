//! Sending chat messages through Helix and publishing the confirmed rows.

use super::*;

/// Sending chat through Helix is serialized per account/channel, and bounded so
/// repeated Ctrl+Enter cannot accumulate an unbounded backlog behind Twitch.
pub(super) const MAX_PENDING_CONFIRMED_SENDS_PER_LANE: usize = 8;

pub(super) struct ConfirmedSendLane {
    pub(super) capacity: Arc<tokio::sync::Semaphore>,
    pub(super) serial: tokio::sync::Mutex<()>,
}

pub(super) static CONFIRMED_SEND_LANES: LazyLock<
    DashMap<(String, String), Arc<ConfirmedSendLane>>,
> = LazyLock::new(DashMap::new);

/// Accepted Helix sends whose IRC echo has not yet supplied final Twitch tags.
/// This is bounded and keyed by the actual Twitch id, never message text.
pub(super) const MAX_CONFIRMED_LOCAL_ECHOES: usize = 2_048;
pub(super) static CONFIRMED_LOCAL_ECHOES: LazyLock<Mutex<LruCache<(String, String), ()>>> =
    LazyLock::new(|| {
        Mutex::new(LruCache::new(
            NonZeroUsize::new(MAX_CONFIRMED_LOCAL_ECHOES).unwrap(),
        ))
    });

/// Return the action body for a `/me <text>` submission.
///
/// Command dispatch is ASCII case-insensitive, so the wire path must accept
/// `/ME` the same way instead of sending it as ordinary text.
pub(super) fn action_message_body(message: &str) -> Option<&str> {
    let prefix = message.get(..4)?;
    prefix.eq_ignore_ascii_case("/me ").then(|| &message[4..])
}

#[derive(Debug, Deserialize)]
pub(super) struct HelixChatSendResponse {
    #[serde(default)]
    pub(super) data: Vec<HelixChatSendResult>,
}

#[derive(Debug, Deserialize)]
pub(super) struct HelixChatSendResult {
    #[serde(default)]
    pub(super) message_id: String,
    pub(super) is_sent: bool,
    #[serde(default)]
    pub(super) drop_reason: Option<HelixChatDropReason>,
}

#[derive(Debug, Deserialize)]
pub(super) struct HelixChatDropReason {
    #[serde(default)]
    pub(super) code: String,
    #[serde(default)]
    pub(super) message: String,
}

/// Strictly classify the authoritative Helix response. A local chat row is
/// allowed only for a positive `is_sent` result with Twitch's real message ID.
/// A failed chat send. `delivery_unknown` marks failures after which Twitch
/// may still have posted the message (network error, 5xx, an unreadable or
/// inconsistent send result, an account switch mid-request, IRC `/me`), so
/// the text must not go back into the draft where a second Enter repeats it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SendError {
    pub message: String,
    pub delivery_unknown: bool,
}

impl SendError {
    pub(super) fn delivery_unknown(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            delivery_unknown: true,
        }
    }
}

impl From<String> for SendError {
    fn from(message: String) -> Self {
        Self {
            message,
            delivery_unknown: false,
        }
    }
}

impl From<&str> for SendError {
    fn from(message: &str) -> Self {
        message.to_string().into()
    }
}

impl std::fmt::Display for SendError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

pub(super) fn accepted_helix_chat_message_id(
    response: HelixChatSendResponse,
) -> Result<String, SendError> {
    let [result] = response.data.as_slice() else {
        return Err(SendError::delivery_unknown(
            "Twitch returned a malformed chat-send result; the message was not shown locally.",
        ));
    };
    if result.is_sent && result.drop_reason.is_some() {
        return Err(SendError::delivery_unknown(
            "Twitch returned an inconsistent chat-send result; the message was not shown locally.",
        ));
    }
    if !result.is_sent {
        let reason = result
            .drop_reason
            .as_ref()
            .map(|reason| {
                crate::backend::ui_actions::composer_send::rejection_reason(
                    &reason.code,
                    &reason.message,
                )
            })
            .unwrap_or_else(|| "Twitch declined the message without a reason".to_string());
        return Err(reason.into());
    }
    let message_id = result.message_id.trim();
    if message_id.is_empty() {
        return Err(SendError::delivery_unknown(
            "Twitch accepted the request without a message ID; the message was not shown locally.",
        ));
    }
    Ok(message_id.to_string())
}

pub(super) fn confirmed_send_lane(account_user_id: &str, channel: &str) -> Arc<ConfirmedSendLane> {
    if CONFIRMED_SEND_LANES.len() > DEDUP_MAX_CHANNELS {
        let connected = CONNECTED_CHANNELS.read();
        CONFIRMED_SEND_LANES.retain(|(lane_account, lane_channel), lane| {
            (lane_account == account_user_id && connected.contains(lane_channel))
                || Arc::strong_count(lane) > 1
        });
    }
    let key = (account_user_id.to_string(), channel.to_lowercase());
    CONFIRMED_SEND_LANES
        .entry(key)
        .or_insert_with(|| {
            Arc::new(ConfirmedSendLane {
                capacity: Arc::new(tokio::sync::Semaphore::new(
                    MAX_PENDING_CONFIRMED_SENDS_PER_LANE,
                )),
                serial: tokio::sync::Mutex::new(()),
            })
        })
        .clone()
}

pub(super) async fn send_confirmed_helix_message(
    channel: &str,
    message: &str,
    reply_parent_message_id: Option<&str>,
    local_reply: Option<ReplyInfo>,
    mut auth: crate::backend::auth::AuthenticatedUserResource,
) -> Result<(), SendError> {
    ensure_authenticated_send_ready()?;
    let channel_key = ensure_channel_send_ready(channel)?;
    if !auth.is_current() {
        return Err("The active Twitch account changed before the message was sent.".into());
    }

    let lane = confirmed_send_lane(&auth.user_id, &channel_key);
    let _pending_permit = lane.capacity.clone().try_acquire_owned().map_err(|_| {
        format!(
            "Too many messages are waiting to be confirmed for #{channel_key}; wait for Twitch before sending more."
        )
    })?;
    let _serial = lane.serial.lock().await;
    ensure_authenticated_send_ready()?;
    ensure_channel_send_ready(&channel_key)?;

    if !auth.is_current() {
        return Err("The active Twitch account changed before the message was sent.".into());
    }
    let broadcaster_id = get_user_id_cached(&channel_key)
        .await
        .map_err(|error| format!("Could not resolve #{channel_key} for Twitch chat: {error}"))?;
    if !auth.is_current() {
        return Err("The active Twitch account changed before the message was sent.".into());
    }

    let mut body = serde_json::json!({
        "broadcaster_id": broadcaster_id,
        "sender_id": auth.user_id.clone(),
        "message": message,
    });
    if let Some(parent_id) = reply_parent_message_id
        .map(str::trim)
        .filter(|id| !id.is_empty())
    {
        body["reply_parent_message_id"] = serde_json::Value::String(parent_id.to_string());
    }

    let mut refreshed_after_unauthorized = false;
    let response = loop {
        let response = TWITCH_HTTP_CLIENT
            .post("https://api.twitch.tv/helix/chat/messages")
            .helix_auth(&auth.access_token)
            .json(&body)
            .send()
            .await
            .map_err(|error| {
                SendError::delivery_unknown(format!(
                    "Twitch could not confirm whether the message was sent: {error}"
                ))
            })?;
        if !auth.is_current() {
            return Err(SendError::delivery_unknown(
                "The active Twitch account changed while Twitch processed the message; it was not shown locally.",
            ));
        }
        if response.status() != reqwest::StatusCode::UNAUTHORIZED || refreshed_after_unauthorized {
            break response;
        }
        refreshed_after_unauthorized = true;
        let detail = crate::backend::http::error_body(response).await;
        let refreshed =
            match crate::backend::auth::recover_helix_unauthorized(&auth.access_token, &detail)
                .await
            {
                Some(_) => crate::backend::auth::capture_authenticated_user_resource()
                    .filter(|refreshed| refreshed.user_id == auth.user_id),
                None => None,
            };
        let Some(refreshed) = refreshed else {
            return Err(format!(
                "Twitch rejected the message ({}): {}",
                reqwest::StatusCode::UNAUTHORIZED,
                crate::backend::bridge::trim_error_detail(&detail, 180, "Unknown network error")
            )
            .into());
        };
        auth = refreshed;
    };
    if !response.status().is_success() {
        let status = response.status();
        let detail = crate::backend::http::error_body(response).await;
        let payload = serde_json::from_str::<serde_json::Value>(&detail).ok();
        let reason = payload
            .as_ref()
            .and_then(|value| value.get("message"))
            .and_then(|value| value.as_str())
            .unwrap_or(&detail)
            .to_string();
        if !status.is_server_error() {
            let account_id = auth.user_id.clone();
            let channel = channel_key.clone();
            let _ = auth.commit_if_current(|| {
                crate::backend::irc::room_state::note_send_rejection(
                    &account_id,
                    &channel,
                    "",
                    &reason,
                );
            });
        }
        return Err(SendError {
            message: match status {
                reqwest::StatusCode::TOO_MANY_REQUESTS => {
                    "Sending too quickly — try again shortly".to_string()
                }
                reqwest::StatusCode::UNPROCESSABLE_ENTITY => {
                    "Message is too long for Twitch".to_string()
                }
                _ => crate::backend::ui_actions::composer_send::rejection_reason("", &reason),
            },
            delivery_unknown: status.is_server_error(),
        });
    }
    let response = response.json::<HelixChatSendResponse>().await.map_err(|error| {
        SendError::delivery_unknown(format!("Twitch returned an invalid chat-send result; the message was not shown locally: {error}"))
    })?;
    if !auth.is_current() {
        return Err(SendError::delivery_unknown(
            "The active Twitch account changed while Twitch processed the message; it was not shown locally.",
        ));
    }
    if let Some(drop_reason) = response
        .data
        .iter()
        .find(|result| !result.is_sent)
        .and_then(|result| result.drop_reason.as_ref())
    {
        let account_id = auth.user_id.clone();
        let channel = channel_key.clone();
        let code = drop_reason.code.clone();
        let text = drop_reason.message.clone();
        let _ = auth.commit_if_current(|| {
            crate::backend::irc::room_state::note_send_rejection(
                &account_id,
                &channel,
                &code,
                &text,
            );
        });
    }
    let message_id = accepted_helix_chat_message_id(response)?;
    let authenticated_user_id = auth.user_id.clone();
    auth.commit_if_current(|| {
        publish_confirmed_message(
            &channel_key,
            message,
            &message_id,
            local_reply,
            &authenticated_user_id,
        );
        crate::backend::irc::room_state::record_confirmed_send(
            &authenticated_user_id,
            &channel_key,
        );
    })
    .ok_or_else(|| {
        SendError::delivery_unknown(
            "The active Twitch account changed while Twitch processed the message; it was not shown locally.",
        )
    })?;
    Ok(())
}

/// Send a message bound to the account that owned the composer submission.
/// The bridge captures this resource before spawning its asynchronous task.
pub(crate) async fn send_message_with_auth(
    channel: &str,
    message: &str,
    auth: crate::backend::auth::AuthenticatedUserResource,
) -> Result<(), SendError> {
    crate::bench::reject_live_outbound_user_action("chat message")?;
    if !auth.is_current() {
        return Err("The active Twitch account changed before the message was sent.".into());
    }
    if let Some(action_body) = action_message_body(message) {
        ensure_authenticated_send_ready()?;
        let channel_key = ensure_channel_send_ready(channel)?;
        let client = get_irc_client().ok_or("IRC client not connected")?;
        client
            .me(channel_key, action_body.to_string())
            .await
            .map_err(|error| {
                SendError::delivery_unknown(format!("Failed to send action message: {error}"))
            })?;
        add_system_message(
            channel.trim_start_matches('#'),
            "Action message submitted over IRC. Delivery is unconfirmed; no local sent row has been added.",
            SystemMessageType::Info,
        );
        return Ok(());
    }

    let channel_key = ensure_channel_send_ready(channel)?;
    send_confirmed_helix_message(&channel_key, message, None, None, auth).await
}

/// Why the active account cannot send Twitch chat right now. Callers that
/// show a short status match on the variant; `Display` is the full
/// explanation for toasts and command errors.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SendBlock {
    Anonymous,
    NeedsReauth,
    NoAccount,
    NoChatToken { username: String },
    Reconnecting,
    Disconnected,
    ClientNotReady,
}

impl std::fmt::Display for SendBlock {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Anonymous => f.write_str(
                "Cannot send Twitch chat while Echo is connected anonymously. Turn off Anonymous in the account switcher, re-authenticate the active Twitch account, or wait for the authenticated reconnect to finish.",
            ),
            Self::NeedsReauth => f.write_str(
                "Cannot send Twitch chat until the active Twitch account is re-authenticated.",
            ),
            Self::NoAccount => f.write_str("Cannot send Twitch chat without an active Twitch account."),
            Self::NoChatToken { username } => write!(
                f,
                "Cannot send Twitch chat as {username}; the active account has no chat token. Re-authenticate the account."
            ),
            Self::Reconnecting => f.write_str(
                "Cannot send Twitch chat while Echo is still connecting or reconnecting to Twitch IRC.",
            ),
            Self::Disconnected => f.write_str(
                "Cannot send Twitch chat while Twitch IRC is disconnected. Wait for Echo to reconnect.",
            ),
            Self::ClientNotReady => {
                f.write_str("Cannot send Twitch chat until the Twitch IRC connection is ready.")
            }
        }
    }
}

impl From<SendBlock> for String {
    fn from(block: SendBlock) -> Self {
        block.to_string()
    }
}

impl From<SendBlock> for SendError {
    fn from(block: SendBlock) -> Self {
        block.to_string().into()
    }
}

pub fn ensure_authenticated_send_ready() -> Result<(), SendBlock> {
    if is_anonymous_mode() {
        return Err(SendBlock::Anonymous);
    }

    if crate::backend::auth::get_needs_reauth() {
        return Err(SendBlock::NeedsReauth);
    }

    let Some(username) = get_username().filter(|username| !username.trim().is_empty()) else {
        return Err(SendBlock::NoAccount);
    };

    if get_token()
        .as_deref()
        .is_none_or(|token| token.trim().is_empty())
    {
        return Err(SendBlock::NoChatToken { username });
    }

    match get_connection_status() {
        ConnectionStatus::Connected => {}
        ConnectionStatus::Reconnecting => return Err(SendBlock::Reconnecting),
        ConnectionStatus::Disconnected => return Err(SendBlock::Disconnected),
    }

    if get_irc_client().is_none() {
        return Err(SendBlock::ClientNotReady);
    }

    Ok(())
}

pub fn ensure_channel_send_ready(channel: &str) -> Result<String, String> {
    let channel_key = channel.trim_start_matches('#').trim().to_lowercase();
    if channel_key.is_empty() {
        return Err("Cannot send Twitch chat without a channel.".to_string());
    }

    if !is_channel_join_confirmed(&channel_key) {
        return Err(format!("Joining #{channel_key} — try again in a moment"));
    }

    if let Some(status) = crate::backend::irc::room_state::current_send_eligibility(&channel_key) {
        return Err(status.block.text(&channel_key));
    }

    Ok(channel_key)
}

pub(super) fn reply_info_for_confirmed_message(
    channel: &str,
    reply_to_msg_id: &str,
    reply_to_user: &str,
    reply_to_display_name: &str,
) -> ReplyInfo {
    let parent_body = CHAT_MESSAGES
        .get(&channel.to_lowercase())
        .and_then(|buffer| {
            buffer
                .iter()
                .find(|m| m.id.as_ref() == reply_to_msg_id)
                .map(|m| m.content.to_string())
        });

    ReplyInfo {
        parent_msg_id: Arc::from(reply_to_msg_id),
        parent_user_login: Arc::from(reply_to_user.to_lowercase()),
        parent_display_name: Arc::from(reply_to_display_name),
        parent_user_id: None,
        parent_msg_body: parent_body.map(Arc::from),
    }
}

/// Build a local row only after Twitch has assigned its canonical ID.
pub(super) fn build_confirmed_message(
    channel: &str,
    message: &str,
    message_id: &str,
    reply: Option<ReplyInfo>,
    authenticated_user_id: &str,
) -> Option<ChatMessage> {
    let username = get_username()?;

    let display_name = crate::backend::settings::get_active_account()
        .and_then(|a| a.display_name)
        .unwrap_or_else(|| username.clone());

    let SelfChannelState { color, badges } = get_self_state(channel).unwrap_or_default();

    let (clean_text, is_action) = if let Some(stripped) = action_message_body(message) {
        (stripped.to_string(), true)
    } else {
        (message.to_string(), false)
    };

    let slots: Vec<_> = parse_to_slots(
        &clean_text,
        channel,
        crate::backend::emotes::EmoteSender::new(Some(authenticated_user_id), &username),
        &FxHashMap::default(),
    );
    let structure_hash = compute_structure_hash(&slots);
    let slots: std::sync::Arc<[_]> = slots.into();

    let chat_msg = ChatMessage {
        id: Arc::from(message_id),
        channel: Arc::from(channel.to_lowercase().as_str()),
        emote_lookup_channel: None,
        emote_lookup_source_id: None,
        sender: Arc::from(username.to_lowercase().as_str()),
        display_name: display_name.into(),
        content: clean_text.clone().into(),
        color: color.map(|c| Arc::from(c.as_str())),
        badges: badges.into_iter().map(|b| Arc::from(b.as_str())).collect(),
        timestamp: Utc::now().timestamp_millis(),
        slots,
        structure_hash,
        reply,
        flags: crate::backend::models::MessageFlags {
            is_action,
            ..Default::default()
        },
        twitch_gifs_tag: None,
        twitch_emotes_tag: None,
        user_id: Some(Arc::from(authenticated_user_id)),
        reply_emote_offset: 0,
    };
    Some(chat_msg)
}

pub(super) fn publish_confirmed_message(
    channel: &str,
    message: &str,
    message_id: &str,
    reply: Option<ReplyInfo>,
    authenticated_user_id: &str,
) {
    let channel_key = channel.trim_start_matches('#').trim().to_lowercase();
    let message_id = message_id.trim();
    if channel_key.is_empty() || message_id.is_empty() {
        return;
    }
    if CHAT_MESSAGES
        .get(&channel_key)
        .is_some_and(|buffer| buffer.contains_id(message_id))
    {
        return;
    }
    let Some(mut chat_msg) = build_confirmed_message(
        &channel_key,
        message,
        message_id,
        reply,
        authenticated_user_id,
    ) else {
        return;
    };
    let mut receipts = CONFIRMED_LOCAL_ECHOES.lock();
    if !mark_message_seen(&channel_key, message_id) {
        return;
    }
    receipts.put((channel_key.clone(), message_id.to_string()), ());
    let visibility_commit = crate::backend::blocked_users::visibility_commit_guard();
    let inserted = {
        let mut buffer = CHAT_MESSAGES
            .entry(channel_key.clone())
            .or_insert_with(MessageBuffer::with_default_capacity);
        insert_confirmed_local_row(&mut buffer, &mut chat_msg)
    };
    if !inserted {
        drop(visibility_commit);
        receipts.pop(&(channel_key, message_id.to_string()));
        return;
    }
    crate::backend::history_cache::record_live_chat_message(channel, &chat_msg);
    bump_chat_version();
    // Notify under the gate, like received rows, so the UI appends rows in
    // buffer order and each keeps the zebra parity it was stored with.
    crate::backend::observers::get_observer_registry().notify(
        channel,
        crate::backend::observers::ChannelEvent::NewMessage(chat_msg),
    );
    drop(visibility_commit);
    drop(receipts);
}

/// Send a reply using Helix's reply-parent field. The local row appears only
/// after Twitch returns the reply's real message ID.
pub async fn send_reply(
    channel: &str,
    message: &str,
    reply_to_msg_id: &str,
    reply_to_user: &str,
) -> Result<(), String> {
    let auth = crate::backend::auth::capture_authenticated_user_resource().ok_or(
        "Cannot send Twitch chat until Echo has the active account identity. Re-authenticate the account.",
    )?;
    send_reply_with_auth(channel, message, reply_to_msg_id, reply_to_user, auth)
        .await
        .map_err(|error| error.message)
}

/// Send a reply bound to the account that owned the composer submission.
pub(crate) async fn send_reply_with_auth(
    channel: &str,
    message: &str,
    reply_to_msg_id: &str,
    reply_to_user: &str,
    auth: crate::backend::auth::AuthenticatedUserResource,
) -> Result<(), SendError> {
    crate::bench::reject_live_outbound_user_action("chat reply")?;
    if !auth.is_current() {
        return Err("The active Twitch account changed before the reply was sent.".into());
    }
    let channel_key = ensure_channel_send_ready(channel)?;
    if let Some(action_body) = action_message_body(message) {
        ensure_authenticated_send_ready()?;
        let client = get_irc_client().ok_or("IRC client not connected")?;
        let reply_target = (channel_key.as_str(), reply_to_msg_id);
        client
            .me_in_reply_to(&reply_target, action_body.to_string())
            .await
            .map_err(|error| {
                SendError::delivery_unknown(format!("Failed to send action reply: {error}"))
            })?;
        add_system_message(
            &channel_key,
            "Action reply submitted over IRC. Delivery is unconfirmed; no local sent row has been added.",
            SystemMessageType::Info,
        );
        return Ok(());
    }

    let reply = reply_info_for_confirmed_message(
        &channel_key,
        reply_to_msg_id,
        reply_to_user,
        reply_to_user,
    );
    send_confirmed_helix_message(
        &channel_key,
        message,
        Some(reply_to_msg_id),
        Some(reply),
        auth,
    )
    .await
}
