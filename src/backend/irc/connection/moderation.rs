//! CLEARCHAT/CLEARMSG handling: deleted messages and moderation rows.

use super::*;

/// Process a CLEARCHAT message (ban/timeout/clear)
pub(super) fn process_clearchat_message(msg: twitch_irc::message::ClearChatMessage) {
    use twitch_irc::message::ClearChatAction;
    let (text, streamer_text, target_user, deleted_sender) = match &msg.action {
        ClearChatAction::UserBanned { user_login, .. } => (
            format!("{} was permanently banned", user_login),
            "A user was permanently banned".to_string(),
            user_login.clone(),
            Some(user_login.as_str()),
        ),
        ClearChatAction::UserTimedOut {
            user_login,
            timeout_length,
            ..
        } => (
            format!(
                "{} timed out for {} seconds",
                user_login,
                timeout_length.as_secs()
            ),
            "A user was timed out".to_string(),
            user_login.clone(),
            Some(user_login.as_str()),
        ),
        ClearChatAction::ChatCleared => (
            "Chat was cleared by a moderator".to_string(),
            "Chat was cleared".to_string(),
            "moderator".to_string(),
            None,
        ),
    };
    let display_text = moderation_system_message_text(&text, &streamer_text);

    crate::log_irc!(info, "Mod: {}: {}", msg.channel_login, text);
    if let ClearChatAction::UserTimedOut {
        user_login,
        timeout_length,
        ..
    } = &msg.action
    {
        add_timeout_system_message(
            &msg.channel_login,
            user_login,
            timeout_length.as_secs(),
            &streamer_text,
        );
    } else {
        add_moderation_system_message(
            &msg.channel_login,
            &display_text,
            &target_user,
            moderation_action_filter_label(&msg.action),
            "",
            "",
        );
    }

    if let Some(target_user_id) = msg.source.tags.0.get("target-user-id") {
        match &msg.action {
            ClearChatAction::UserBanned { .. } => {
                crate::backend::irc::room_state::note_self_moderation(
                    target_user_id,
                    &msg.channel_login,
                    None,
                );
            }
            ClearChatAction::UserTimedOut { timeout_length, .. } => {
                let sent_at = msg
                    .source
                    .tags
                    .0
                    .get("tmi-sent-ts")
                    .and_then(|timestamp| timestamp.parse::<i64>().ok())
                    .unwrap_or_else(|| Utc::now().timestamp_millis());
                let elapsed_ms =
                    Utc::now().timestamp_millis().saturating_sub(sent_at).max(0) as u64;
                let remaining =
                    timeout_length.saturating_sub(std::time::Duration::from_millis(elapsed_ms));
                crate::backend::irc::room_state::note_self_moderation(
                    target_user_id,
                    &msg.channel_login,
                    Some(remaining),
                );
            }
            ClearChatAction::ChatCleared => {}
        }
    }

    let (action_type, timeout_secs) = match &msg.action {
        ClearChatAction::UserBanned { .. } => (ModActionType::Ban, None),
        ClearChatAction::UserTimedOut { timeout_length, .. } => {
            (ModActionType::Timeout, Some(timeout_length.as_secs()))
        }
        ClearChatAction::ChatCleared => (ModActionType::Clear, None),
    };
    add_mod_action(
        ModAction::new(
            action_type,
            target_user,
            msg.channel_login.as_str(),
            display_text,
            Utc::now().timestamp_millis(),
        )
        .with_timeout_secs(timeout_secs),
    );

    apply_deleted_message_display_for_sender(&msg.channel_login, deleted_sender);
}

/// Feed a raw CLEARMSG or CLEARCHAT line through the live moderation path.
/// Only the chat replay fixture calls this, so a run without an account can
/// exercise deletions, timeouts, bans and chat clears. Returns whether the
/// line was one of those commands.
pub(crate) fn replay_raw_moderation_line(raw: &str) -> bool {
    let Ok(irc) = twitch_irc::message::IRCMessage::parse(raw) else {
        return false;
    };
    match ServerMessage::try_from(irc) {
        Ok(ServerMessage::ClearMsg(msg)) => {
            process_clearmsg_message(msg);
            true
        }
        Ok(ServerMessage::ClearChat(msg)) => {
            process_clearchat_message(msg);
            true
        }
        _ => false,
    }
}

/// Process a CLEARMSG message (single message deletion)
pub(super) fn process_clearmsg_message(msg: twitch_irc::message::ClearMsgMessage) {
    let original_message = find_buffered_message_by_id(&msg.channel_login, &msg.message_id);
    let text = format!("Message from {} was deleted", msg.sender_login);
    // The notice carries the removed text so it stays readable
    // in a split that hides deleted rows. The log line never includes it.
    let notice = match original_message
        .as_ref()
        .map(|original| deleted_text_preview(&original.content))
    {
        Some(preview) if !preview.is_empty() => format!("{text}: {preview}"),
        _ => text.clone(),
    };
    let display_text = moderation_system_message_text(&notice, "A message was deleted");
    crate::log_irc!(
        info,
        "Mod: {}: {} (id: {})",
        msg.channel_login,
        text,
        msg.message_id
    );
    add_moderation_system_message(
        &msg.channel_login,
        &display_text,
        &msg.sender_login,
        "delete",
        "",
        "",
    );

    add_mod_action(ModAction::new(
        ModActionType::Delete,
        msg.sender_login.as_str(),
        msg.channel_login.as_str(),
        display_text.as_str(),
        Utc::now().timestamp_millis(),
    ));

    crate::backend::history_cache::record_readable_deleted_message(
        &msg.channel_login,
        &msg.message_id,
        &msg.sender_login,
        original_message.as_ref(),
    );

    apply_deleted_message_display(&msg.channel_login, &msg.message_id);
}

/// Longest removed-text preview a deletion notice carries.
pub(super) const DELETED_PREVIEW_LIMIT: usize = 100;

pub(super) fn deleted_text_preview(content: &str) -> String {
    let mut chars = content.trim().chars();
    let mut preview: String = chars.by_ref().take(DELETED_PREVIEW_LIMIT).collect();
    if chars.next().is_some() {
        preview.push('…');
    }
    preview
}

pub(super) fn find_buffered_message_by_id(channel: &str, message_id: &str) -> Option<ChatMessage> {
    let channel_key = channel.trim_start_matches('#').to_lowercase();
    if channel_key.is_empty() || message_id.trim().is_empty() {
        return None;
    }

    CHAT_MESSAGES.get(&channel_key).and_then(|buffer| {
        buffer
            .iter()
            .find(|msg| msg.id.as_ref() == message_id)
            .cloned()
    })
}

pub(super) fn moderation_system_message_text(text: &str, streamer_text: &str) -> String {
    if crate::backend::streamer_mode::should_hide_mod_actions() {
        streamer_text.to_string()
    } else {
        text.to_string()
    }
}

pub(super) fn moderation_action_filter_label(
    action: &twitch_irc::message::ClearChatAction,
) -> &'static str {
    match action {
        twitch_irc::message::ClearChatAction::UserBanned { .. } => "ban",
        twitch_irc::message::ClearChatAction::UserTimedOut { .. } => "timeout",
        twitch_irc::message::ClearChatAction::ChatCleared => "clear",
    }
}

/// Compact duration: "10m", "1d", "1h 30m".
pub(super) fn format_timeout_duration(total_seconds: u64) -> String {
    const UNITS: [(u64, &str); 4] = [(86_400, "d"), (3_600, "h"), (60, "m"), (1, "s")];
    let mut remaining = total_seconds;
    let mut parts = Vec::new();
    for (unit_seconds, suffix) in UNITS {
        let count = remaining / unit_seconds;
        if count > 0 {
            parts.push(format!("{count}{suffix}"));
            remaining %= unit_seconds;
        }
    }
    if parts.is_empty() {
        "0s".to_string()
    } else {
        parts.join(" ")
    }
}

pub(super) fn timeout_row_text(
    user_login: &str,
    seconds: u64,
    streamer_text: &str,
    count: u32,
) -> String {
    let text = format!(
        "{} timed out for {}",
        user_login,
        format_timeout_duration(seconds)
    );
    let text = moderation_system_message_text(&text, streamer_text);
    if count > 1 {
        format!("{text} ({count} times)")
    } else {
        text
    }
}

/// Repeated timeouts of one user within this window and row distance merge
/// into one row.
pub(super) const TIMEOUT_STACK_WINDOW_MS: i64 = 5_000;
pub(super) const TIMEOUT_STACK_RECENT_ROWS: usize = 20;

pub(super) struct RecentTimeoutRow {
    pub(super) row_id: String,
    pub(super) count: u32,
    pub(super) at_ms: i64,
}

pub(super) static RECENT_TIMEOUT_ROWS: LazyLock<
    Mutex<HashMap<(String, String), RecentTimeoutRow>>,
> = LazyLock::new(|| Mutex::new(HashMap::new()));

pub(super) fn add_timeout_system_message(
    channel: &str,
    user_login: &str,
    seconds: u64,
    streamer_text: &str,
) {
    let channel_key = channel.to_lowercase();
    let now_ms = Utc::now().timestamp_millis();
    let key = (channel_key.clone(), user_login.to_lowercase());

    let mut recent = RECENT_TIMEOUT_ROWS.lock();
    recent.retain(|_, row| now_ms.saturating_sub(row.at_ms) <= TIMEOUT_STACK_WINDOW_MS);
    if let Some(row) = recent.get_mut(&key) {
        let count = row.count.saturating_add(1);
        let text = timeout_row_text(user_login, seconds, streamer_text, count);
        let verdict = moderation_row_filter_verdict(channel, &text, user_login, "timeout", "", "");
        let visibility_commit = crate::backend::blocked_users::visibility_commit_guard();
        let updated = CHAT_MESSAGES
            .get_mut(&channel_key)
            .is_some_and(|mut buffer| {
                if !buffer
                    .iter_recent(TIMEOUT_STACK_RECENT_ROWS)
                    .any(|message| message.id.as_ref() == row.row_id)
                {
                    return false;
                }
                match &verdict {
                    None => buffer.remove_by_id(&row.row_id),
                    Some(highlight_type) => buffer.update_by_id(&row.row_id, |message| {
                        let slots = vec![MessageSlot::text(text.as_str())];
                        message.structure_hash = compute_structure_hash(&slots);
                        message.slots = slots.into();
                        message.content = Arc::from(text.as_str());
                        message.flags.highlight_type = highlight_type.clone();
                    }),
                }
            });
        drop(visibility_commit);
        if updated {
            row.count = count;
            row.at_ms = now_ms;
            let row_id: Arc<str> = Arc::from(row.row_id.as_str());
            if verdict.is_none() {
                recent.remove(&key);
            }
            drop(recent);
            crate::components::chat::canvas_renderer::global_layout_cache::clear_layouts_for_messages(
                &HashSet::from([row_id]),
            );
            crate::backend::history_cache::schedule_visible_channel_history_persist(&channel_key);
            bump_chat_version();
            crate::backend::observers::get_observer_registry().notify(
                &channel_key,
                crate::backend::observers::ChannelEvent::MessagesRetokenized,
            );
            return;
        }
    }

    let row_id = Uuid::new_v4().to_string();
    recent.insert(
        key,
        RecentTimeoutRow {
            row_id: row_id.clone(),
            count: 1,
            at_ms: now_ms,
        },
    );
    drop(recent);
    add_moderation_system_message_with_id(
        channel,
        &timeout_row_text(user_login, seconds, streamer_text, 1),
        user_login,
        "timeout",
        "",
        "",
        Some(row_id),
    );
}

pub(super) fn add_moderation_system_message(
    channel: &str,
    text: &str,
    author_login: &str,
    event_action: &str,
    moderator_login: &str,
    event_reason: &str,
) {
    add_moderation_system_message_with_id(
        channel,
        text,
        author_login,
        event_action,
        moderator_login,
        event_reason,
        None,
    );
}

pub(super) fn add_moderation_system_message_with_id(
    channel: &str,
    text: &str,
    author_login: &str,
    event_action: &str,
    moderator_login: &str,
    event_reason: &str,
    row_id: Option<String>,
) {
    let Some(highlight_type) = moderation_row_filter_verdict(
        channel,
        text,
        author_login,
        event_action,
        moderator_login,
        event_reason,
    ) else {
        crate::log_irc!(debug, "Filter: Hiding moderation system row in {}", channel);
        return;
    };
    add_system_row(SystemRow {
        highlight: highlight_type,
        id: row_id,
        ..SystemRow::new(channel, text, SystemMessageType::Info)
    });
}

/// Run the channel filters over a moderation row: `None` hides it, otherwise
/// the highlight it should carry.
pub(super) fn moderation_row_filter_verdict(
    channel: &str,
    text: &str,
    author_login: &str,
    event_action: &str,
    moderator_login: &str,
    event_reason: &str,
) -> Option<Option<HighlightType>> {
    let filter_ctx = TestContext {
        content: text.to_string(),
        author: author_login.to_string(),
        author_login: author_login.to_lowercase(),
        moderator_name: moderator_login.to_string(),
        moderator_login: moderator_login.to_lowercase(),
        target_name: author_login.to_string(),
        target_login: author_login.to_lowercase(),
        channel_name: channel.to_string(),
        event_action: event_action.to_string(),
        event_reason: event_reason.to_string(),
        event_source: "irc".to_string(),
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
        return None;
    }

    Some(
        filter_highlight_match
            .map(|pattern| HighlightType::Phrase { pattern })
            .or_else(|| filter_highlighted.then_some(HighlightType::HighlightedMessage)),
    )
}

/// Mark one row deleted. The row keeps its original text: whether it fades,
/// collapses or disappears is each split's choice, so a split can reveal it later.
pub(super) fn apply_deleted_message_display(channel: &str, message_id: &str) {
    let channel_key = channel.trim_start_matches('#').to_lowercase();
    if channel_key.is_empty() {
        return;
    }

    let affected_ids: HashSet<String> = std::iter::once(message_id.to_string()).collect();

    let visibility_commit = crate::backend::blocked_users::visibility_commit_guard();
    let changed = CHAT_MESSAGES
        .get_mut(&channel_key)
        .map(|mut buffer| {
            buffer.update_by_id(message_id, |msg| {
                msg.flags.is_deleted = true;
            })
        })
        .unwrap_or(false);
    drop(visibility_commit);

    if changed {
        crate::components::chat::canvas_renderer::global_layout_cache::clear_layouts_for_messages(
            &affected_ids,
        );
        crate::backend::history_cache::schedule_visible_channel_history_persist(&channel_key);
        bump_chat_version();
        crate::backend::observers::get_observer_registry().notify(
            &channel_key,
            crate::backend::observers::ChannelEvent::MessagesRetokenized,
        );
    }
}

/// Mark every ordinary row authored by `sender` deleted. A sender-less
/// CLEARCHAT is Twitch's full chat-clear event and therefore targets every
/// ordinary chat row while preserving the moderation notice and other system
/// rows. Rows keep their text; each split decides how to present them.
pub(super) fn apply_deleted_message_display_for_sender(channel: &str, sender: Option<&str>) {
    use crate::state::MessageReconcileAction;

    let channel_key = channel.trim_start_matches('#').to_lowercase();
    if channel_key.is_empty() {
        return;
    }

    let mut mutated_ids = HashSet::new();
    let visibility_commit = crate::backend::blocked_users::visibility_commit_guard();
    let changed = CHAT_MESSAGES
        .get_mut(&channel_key)
        .map(|mut buffer| {
            let result = buffer.reconcile(|message| {
                let is_target = !message.flags.is_system
                    && sender.is_none_or(|login| message.sender.eq_ignore_ascii_case(login));
                if !is_target || message.flags.is_deleted {
                    return MessageReconcileAction::Keep;
                }

                message.flags.is_deleted = true;
                if sender.is_some() && !message.id.is_empty() {
                    mutated_ids.insert(message.id.to_string());
                }
                MessageReconcileAction::Mutated
            });
            result.removed != 0 || result.mutated != 0
        })
        .unwrap_or(false);
    drop(visibility_commit);

    if changed {
        if sender.is_none() {
            crate::components::chat::canvas_renderer::global_layout_cache::clear_layouts_for_channel(
                &channel_key,
            );
        } else {
            crate::components::chat::canvas_renderer::global_layout_cache::clear_layouts_for_messages(
                &mutated_ids,
            );
        }
        crate::backend::history_cache::schedule_visible_channel_history_persist(&channel_key);
        bump_chat_version();
        crate::backend::observers::get_observer_registry().notify(
            &channel_key,
            crate::backend::observers::ChannelEvent::MessagesRetokenized,
        );
    }
}
