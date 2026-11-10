//! System rows: status, errors, AutoMod holds and local messages.

use super::*;

/// Add a system message to a channel
pub fn add_system_message(channel: &str, text: &str, msg_type: SystemMessageType) {
    let Some(highlight_type) = generic_system_message_filter_highlight(channel, text, &msg_type)
    else {
        return;
    };
    add_system_row(SystemRow {
        highlight: highlight_type,
        ..SystemRow::new(channel, text, msg_type)
    });
}

#[derive(Clone, Copy, Debug, Default)]
pub struct SystemMessageFlagOverrides {
    pub is_automod: bool,
    /// AutoMod hold awaiting a moderator decision (renders inline Allow/Deny).
    pub is_automod_pending: bool,
    pub is_suspicious_user: bool,
    pub is_restricted: bool,
    pub is_monitored: bool,
}

/// A system row for [`add_system_row`]. Start from [`SystemRow::new`] and set
/// the fields that differ.
pub struct SystemRow<'a> {
    pub channel: &'a str,
    pub text: &'a str,
    pub kind: SystemMessageType,
    pub highlight: Option<HighlightType>,
    pub flags: SystemMessageFlagOverrides,
    /// Pins the row id; a blank id is ignored and a row whose pinned id is
    /// already buffered is not added again. AutoMod hold rows key on
    /// [`automod_row_id`] so their inline Allow/Deny hitboxes and a later
    /// `automod.message.update` can target the row.
    pub id: Option<String>,
}

impl<'a> SystemRow<'a> {
    pub fn new(channel: &'a str, text: &'a str, kind: SystemMessageType) -> Self {
        Self {
            channel,
            text,
            kind,
            highlight: None,
            flags: SystemMessageFlagOverrides::default(),
            id: None,
        }
    }
}

/// Add a system row as given, without running the channel filters.
pub fn add_system_row(row: SystemRow<'_>) {
    let SystemRow {
        channel,
        text,
        kind: msg_type,
        highlight: highlight_type,
        flags: flag_overrides,
        id: explicit_id,
    } = row;
    use crate::backend::slots::MessageSlot;

    let slots: Vec<MessageSlot> = if matches!(&msg_type, SystemMessageType::UploadSuccess { .. }) {
        parse_to_slots(
            text,
            channel,
            crate::backend::emotes::EmoteSender::NONE,
            &FxHashMap::default(),
        )
    } else {
        vec![MessageSlot::text(text)]
    };
    let structure_hash = compute_structure_hash(&slots);
    let slots: std::sync::Arc<[_]> = slots.into();

    let pinned_id = explicit_id.filter(|id| !id.trim().is_empty());
    let id_is_pinned = pinned_id.is_some();
    let id: Arc<str> = pinned_id
        .map(Arc::from)
        .unwrap_or_else(|| Uuid::new_v4().to_string().into());
    let sender = Arc::from("System");
    let display_name = Arc::from("System");
    let user_id = None;
    let timestamp = Utc::now().timestamp_millis();

    let mut msg = ChatMessage {
        id,
        channel: Arc::from(channel),
        emote_lookup_channel: None,
        emote_lookup_source_id: None,
        sender,
        display_name,
        content: Arc::from(text),
        color: None,
        badges: smallvec::SmallVec::new(),
        timestamp,
        slots,
        structure_hash,
        reply: None,
        flags: crate::backend::models::MessageFlags {
            highlight_type,
            is_system: true,
            system_type: Some(msg_type),
            is_automod: flag_overrides.is_automod,
            is_automod_pending: flag_overrides.is_automod_pending,
            is_suspicious_user: flag_overrides.is_suspicious_user,
            is_restricted: flag_overrides.is_restricted,
            is_monitored: flag_overrides.is_monitored,
            ..Default::default()
        },
        twitch_gifs_tag: None,
        twitch_emotes_tag: None,
        user_id,
        reply_emote_offset: 0,
    };

    if is_channel_closed(channel) {
        return;
    }

    {
        let mut buffer = CHAT_MESSAGES
            .entry(channel.to_string())
            .or_insert_with(MessageBuffer::with_default_capacity);
        if id_is_pinned && buffer.contains_id(&msg.id) {
            return;
        }
        buffer.push_stamped(&mut msg);
    }
    crate::backend::history_cache::record_live_chat_message(channel, &msg);

    bump_chat_version();
    crate::backend::observers::get_observer_registry().notify(
        channel,
        crate::backend::observers::ChannelEvent::NewMessage(msg),
    );
}

/// Prefix that keeps AutoMod hold rows out of Twitch's real chat-message id
/// namespace. Sharing the held id would make a CLEARMSG/mod-delete for that
/// message resolve to the AutoMod system row instead of the user's message,
/// and would let any later message carrying the id inherit the hold row's
/// cached layout.
pub const AUTOMOD_ROW_ID_PREFIX: &str = "automod_";

/// Row id for the inline AutoMod hold on `held_id`.
pub fn automod_row_id(held_id: &str) -> String {
    format!("{}{}", AUTOMOD_ROW_ID_PREFIX, held_id.trim())
}

/// Recover the bare Twitch message id from an AutoMod row id. The Helix
/// manage-held-message endpoint only accepts the un-namespaced id.
pub fn automod_held_message_id(row_id: &str) -> &str {
    row_id
        .strip_prefix(AUTOMOD_ROW_ID_PREFIX)
        .unwrap_or(row_id)
        .trim()
}

/// Resolve an inline AutoMod hold row in place when its `automod.message.update`
/// arrives: swap the row text for the resolved outcome and clear the pending
/// state so the renderer stops drawing the Allow/Deny actions, rather than
/// appending a second row. Returns true when a held row with `held_id` existed,
/// so the caller can skip inserting a duplicate resolved row.
pub fn resolve_automod_message(
    channel: &str,
    held_id: &str,
    text: &str,
    flag_overrides: SystemMessageFlagOverrides,
) -> bool {
    use crate::backend::slots::MessageSlot;

    let held_id = held_id.trim();
    if held_id.is_empty() {
        return false;
    }
    let row_id = automod_row_id(held_id);
    let resolved_text = text.to_string();

    let changed = CHAT_MESSAGES
        .get_mut(channel)
        .map(|mut buffer| {
            buffer.update_by_id(row_id.as_str(), |msg| {
                let slots: Vec<MessageSlot> = vec![MessageSlot::text(resolved_text.as_str())];
                msg.structure_hash = compute_structure_hash(&slots);
                msg.slots = slots.into();
                msg.content = Arc::from(resolved_text.as_str());
                msg.flags.is_automod = flag_overrides.is_automod;
                msg.flags.is_automod_pending = false;
                msg.flags.is_suspicious_user = flag_overrides.is_suspicious_user;
                msg.flags.is_restricted = flag_overrides.is_restricted;
                msg.flags.is_monitored = flag_overrides.is_monitored;
            })
        })
        .unwrap_or(false);

    if changed {
        bump_chat_version();
        crate::backend::observers::get_observer_registry().notify(
            channel,
            crate::backend::observers::ChannelEvent::MessagesRetokenized,
        );
    }
    changed
}

pub(super) fn generic_system_message_filter_highlight(
    channel: &str,
    text: &str,
    msg_type: &SystemMessageType,
) -> Option<Option<HighlightType>> {
    let filter_ctx = TestContext {
        content: text.to_string(),
        author: "System".to_string(),
        author_login: "system".to_string(),
        channel_name: channel.trim_start_matches('#').to_string(),
        event_action: system_message_filter_label(msg_type).to_string(),
        event_source: "echo".to_string(),
        event_status: system_message_filter_status(msg_type).to_string(),
        event_detail: system_message_filter_detail(msg_type, text),
        is_system: true,
        channel_watching: true,
        channel_live: crate::backend::bridge::get_cached_channel_live_status(channel),
        ..Default::default()
    };

    let mut should_hide = false;
    let mut filter_highlight_match: Option<String> = None;
    let mut filter_highlighted = false;
    for result in FILTER_STORE.test_message(&filter_ctx) {
        if !result.matched {
            continue;
        }

        match result.action {
            FilterAction::Hide => {
                should_hide = true;
                break;
            }
            FilterAction::Highlight => {
                filter_highlighted = true;
                if filter_highlight_match.is_none() {
                    filter_highlight_match =
                        result.matched_text.filter(|text| !text.trim().is_empty());
                }
            }
            FilterAction::Allow => {
                should_hide = false;
                filter_highlighted = false;
                filter_highlight_match = None;
                break;
            }
            FilterAction::Replace => {}
        }
    }

    if should_hide {
        crate::log_irc!(debug, "Filter: Hiding generic system row in {}", channel);
        return None;
    }

    Some(
        filter_highlight_match
            .map(|pattern| HighlightType::Phrase { pattern })
            .or_else(|| filter_highlighted.then_some(HighlightType::HighlightedMessage)),
    )
}

pub(super) fn system_message_filter_label(msg_type: &SystemMessageType) -> &'static str {
    match msg_type {
        SystemMessageType::UploadStart => "upload_start",
        SystemMessageType::UploadSuccess { .. } => "upload_success",
        SystemMessageType::UploadError { .. } => "upload_error",
        SystemMessageType::Info => "info",
        SystemMessageType::Error { .. } => "error",
        SystemMessageType::ConnectionStatus { .. } => "connection",
    }
}

pub(super) fn system_message_filter_status(msg_type: &SystemMessageType) -> &str {
    match msg_type {
        SystemMessageType::UploadStart => "start",
        SystemMessageType::UploadSuccess { .. } => "success",
        SystemMessageType::UploadError { .. } => "error",
        SystemMessageType::Info => "info",
        SystemMessageType::Error { .. } => "error",
        SystemMessageType::ConnectionStatus { status } => status.as_str(),
    }
}

pub(super) fn system_message_filter_detail(msg_type: &SystemMessageType, text: &str) -> String {
    match msg_type {
        SystemMessageType::UploadSuccess { url, delete_url } => delete_url
            .as_ref()
            .map(|delete_url| format!("{url} {delete_url}"))
            .unwrap_or_else(|| url.clone()),
        SystemMessageType::UploadError { error } => error.clone(),
        SystemMessageType::Error { message } => message.clone(),
        _ => text.to_string(),
    }
}

pub(super) fn add_connection_status_message(channel: &str, text: &str, status: &str) {
    add_system_message(
        channel,
        text,
        SystemMessageType::ConnectionStatus {
            status: status.to_string(),
        },
    );
}

pub(super) fn add_history_error_message(channel: &str, prefix: &str, error: &str) {
    let detail = crate::backend::bridge::trim_error_detail(error, 180, "Unknown network error");
    let text = format!("{prefix} {detail}");
    add_system_message(
        channel,
        &text,
        SystemMessageType::Error {
            message: text.clone(),
        },
    );
}

/// Clear locally displayed messages for a channel without sending a Twitch
/// moderation command (`/clearmessages`).
pub fn clear_channel_messages(channel: &str) {
    let channel_key = channel.trim_start_matches('#').to_lowercase();
    if channel_key.is_empty() {
        return;
    }

    let visibility_commit = crate::backend::blocked_users::visibility_commit_guard();
    if let Some(mut buffer) = CHAT_MESSAGES.get_mut(&channel_key) {
        buffer.replace_all(Vec::new());
    }
    drop(visibility_commit);

    crate::backend::history_cache::schedule_visible_channel_history_persist(&channel_key);
    bump_chat_version();
    crate::backend::observers::get_observer_registry().notify(
        &channel_key,
        crate::backend::observers::ChannelEvent::HistoryLoaded(Arc::new(Vec::new())),
    );
}

/// Insert a locally constructed chat message and notify current observers.
pub fn insert_local_message(channel: &str, mut message: ChatMessage) {
    let channel_key = channel.trim_start_matches('#').to_lowercase();
    if channel_key.is_empty() {
        return;
    }

    let visibility_commit = crate::backend::blocked_users::visibility_commit_guard();
    CHAT_MESSAGES
        .entry(channel_key.clone())
        .or_insert_with(MessageBuffer::with_default_capacity)
        .push_stamped(&mut message);

    bump_chat_version();
    // Notify under the gate, like received rows, so the UI appends rows in
    // buffer order and each keeps the zebra parity it was stored with.
    crate::backend::observers::get_observer_registry().notify(
        &channel_key,
        crate::backend::observers::ChannelEvent::NewMessage(message),
    );
    drop(visibility_commit);
}
