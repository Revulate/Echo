//! USERNOTICE ingestion (subs, raids, announcements and other notices).

use super::*;

/// Process a USERNOTICE message (sub/raid/etc)
pub(super) fn process_usernotice_message(msg: twitch_irc::message::UserNoticeMessage) {
    if should_hide_blocked_channel_message(
        Some(&msg.sender.id),
        &msg.sender.login,
        &msg.channel_login,
    ) {
        crate::log_irc!(
            debug,
            "Blocked users: Hiding USERNOTICE from {} in #{}",
            msg.sender.login,
            msg.channel_login
        );
        return;
    }

    let msg_id = msg.source.tags.0.get("msg-id").cloned().unwrap_or_default();

    let tag_value = |key: &str| msg.source.tags.0.get(key).cloned();
    let tags = UsernoticeTags {
        msg_id: msg_id.clone(),
        ritual_name: tag_value("ritual-name"),
        watch_streak_months: tag_value("watch-streak-months").and_then(|v| v.parse().ok()),
        watch_streak: tag_value("watch-streak"),
        milestone_category: tag_value("msg-param-category"),
        milestone_value: tag_value("msg-param-value").and_then(|value| value.parse().ok()),
        modiversary_months: tag_value("msg-param-months").and_then(|value| value.parse().ok()),
    };
    let mut highlight_type = usernotice_core::highlight_kind(&tags).map(usernotice_highlight_type);
    if matches!(highlight_type, Some(HighlightType::Announcement { .. })) {
        let color = tag_value("msg-param-color").filter(|value| !value.trim().is_empty());
        highlight_type = Some(HighlightType::Announcement { color });
    }
    let event_type = usernotice_core::event_type_label(&msg_id);
    let authored_text = msg
        .message_text
        .as_deref()
        .filter(|text| !text.trim().is_empty());
    let system_text = if msg_id == "announcement" && msg.system_message.trim().is_empty() {
        Some("Announcement")
    } else {
        (!msg.system_message.trim().is_empty()).then_some(msg.system_message.as_str())
    };
    if authored_text.is_none() && system_text.is_none() {
        return;
    }

    let filter_text = match (system_text, authored_text) {
        (Some(system), Some(content)) => format!("{system} {content}"),
        (Some(system), None) => system.to_string(),
        (None, Some(content)) => content.to_string(),
        (None, None) => unreachable!("empty USERNOTICE returned above"),
    };
    let sub_length = msg
        .badge_info
        .iter()
        .find(|badge| badge.name == "subscriber")
        .and_then(|badge| badge.version.parse().ok());
    let bits_amount = tag_value("msg-param-threshold")
        .and_then(|value| value.parse::<u32>().ok())
        .unwrap_or(0);
    let color = msg
        .name_color
        .map(|color| format_color_rgb(color.r, color.g, color.b).to_string());
    let filter_ctx = TestContext {
        content: filter_text.clone(),
        author: msg.sender.login.clone(),
        author_login: msg.sender.login.clone(),
        author_user_id: msg.sender.id.clone(),
        author_color: color.clone(),
        badges: badge_names_for_filter(msg.badges.iter().map(|badge| badge.name.as_str())),
        channel_name: msg.channel_login.clone(),
        bits_amount,
        sub_length,
        is_sub: msg.badges.iter().any(|badge| badge.name == "subscriber"),
        is_announcement: matches!(
            highlight_type.as_ref(),
            Some(HighlightType::Announcement { .. })
        ),
        is_system: authored_text.is_none(),
        is_highlighted: highlight_type.is_some(),
        is_cheer: matches!(highlight_type.as_ref(), Some(HighlightType::BitsBadge)),
        is_sub_message: matches!(highlight_type.as_ref(), Some(HighlightType::Subscription)),
        is_watch_streak: matches!(highlight_type.as_ref(), Some(HighlightType::WatchStreak)),
        channel_watching: true,
        channel_live: crate::backend::bridge::get_cached_channel_live_status(&msg.channel_login),
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
        crate::log_irc!(
            debug,
            "Filter: Hiding {} USERNOTICE in {}",
            event_type,
            msg.channel_login
        );
        return;
    }
    if highlight_type.is_none() {
        highlight_type = filter_highlight_match
            .map(|pattern| HighlightType::Phrase { pattern })
            .or_else(|| filter_highlighted.then_some(HighlightType::HighlightedMessage));
    }

    let display_name = msg
        .source
        .tags
        .0
        .get("display-name")
        .cloned()
        .filter(|name| !name.trim().is_empty())
        .unwrap_or_else(|| msg.sender.name.clone());
    let base_id = if msg.message_id.trim().is_empty() {
        Uuid::new_v4().to_string()
    } else {
        msg.message_id.clone()
    };
    let timestamp = msg
        .source
        .tags
        .0
        .get("tmi-sent-ts")
        .and_then(|value| value.parse::<i64>().ok())
        .unwrap_or_else(|| msg.server_timestamp.timestamp_millis());

    let mut rows = Vec::with_capacity(2);
    if let Some(content) = authored_text {
        rows.push(build_usernotice_content_row(
            &msg,
            content,
            &display_name,
            &format!("{base_id}:content"),
            timestamp,
            highlight_type.clone(),
        ));
    }
    if let Some(system) = system_text {
        let system_id = base_id.clone();
        rows.push(build_usernotice_system_row(
            &msg,
            system,
            &display_name,
            system_id,
            timestamp,
            highlight_type.clone(),
        ));
    }

    let visibility_commit = crate::backend::blocked_users::visibility_commit_guard();
    let blocked_visibility = blocked_channel_visibility_snapshot(&msg.channel_login);
    if blocked_visibility.should_hide_sender(Some(&msg.sender.id), &msg.sender.login) {
        crate::log_irc!(
            debug,
            "Blocked users: Hiding USERNOTICE from {} after final visibility recheck in #{}",
            msg.sender.login,
            msg.channel_login
        );
        return;
    }
    let initial_visibility_pending = crate::backend::blocked_users::initial_visibility_pending();
    let inserted = {
        let mut buffer = CHAT_MESSAGES
            .entry(msg.channel_login.clone())
            .or_insert_with(MessageBuffer::with_default_capacity);
        rows.retain(|row| !buffer.contains_id(row.id.as_ref()));
        if initial_visibility_pending
            && crate::backend::blocked_users::pending_channel_capacity_remaining_commit_held(
                &visibility_commit,
            ) < rows.len()
        {
            pending_channel_row_capacity_available_commit_held(&visibility_commit);
            return;
        }
        for row in &mut rows {
            buffer.push_stamped(row);
        }
        rows
    };
    if inserted.is_empty() {
        return;
    }

    if initial_visibility_pending {
        for row in &inserted {
            defer_buffered_row_visibility_commit_held(
                &visibility_commit,
                &msg.channel_login,
                row.id.as_ref(),
            );
        }
        drop(visibility_commit);
        return;
    }

    crate::log_irc!(
        info,
        "Event: {} in {}: {}",
        event_type,
        msg.channel_login,
        filter_text
    );
    if let Some(sound) = sound_for_highlight_type(highlight_type.as_ref()) {
        SOUND_STORE.play_sound_for_channel(sound, &msg.channel_login);
    }
    if authored_text.is_some() {
        if let Some(color) = color.as_deref() {
            cache_user_color(&msg.sender.login, color);
        }
        track_chatter(&msg.channel_login, &msg.sender.login, &display_name, color);
    }
    for row in &inserted {
        crate::backend::history_cache::record_live_chat_message(&msg.channel_login, row);
    }
    bump_chat_version();
    for row in inserted {
        crate::backend::observers::get_observer_registry().notify(
            &msg.channel_login,
            crate::backend::observers::ChannelEvent::NewMessage(row),
        );
    }
    drop(visibility_commit);
}

pub(super) fn build_usernotice_content_row(
    msg: &twitch_irc::message::UserNoticeMessage,
    raw_content: &str,
    display_name: &str,
    id: &str,
    timestamp: i64,
    highlight_type: Option<HighlightType>,
) -> ChatMessage {
    let (content, is_action) = normalize_message_text(raw_content);
    let action_prefix_chars = if is_action {
        CTCP_ACTION_PREFIX_CHARS
    } else {
        0
    };
    let twitch_emotes: FxHashMap<usize, (String, usize)> = msg
        .emotes
        .iter()
        .filter_map(|emote| {
            if emote.char_range.end <= action_prefix_chars {
                return None;
            }
            Some((
                emote.char_range.start.saturating_sub(action_prefix_chars),
                (
                    emote.id.clone(),
                    emote.char_range.end.saturating_sub(action_prefix_chars),
                ),
            ))
        })
        .collect();
    let slots: Vec<_> = parse_to_slots(
        &content,
        &msg.channel_login,
        crate::backend::emotes::EmoteSender::new(Some(&msg.sender.id), &msg.sender.login),
        &twitch_emotes,
    );
    let structure_hash = compute_structure_hash(&slots);

    ChatMessage {
        id: Arc::from(id),
        channel: Arc::from(msg.channel_login.as_str()),
        emote_lookup_channel: None,
        emote_lookup_source_id: None,
        sender: Arc::from(msg.sender.login.as_str()),
        display_name: Arc::from(display_name),
        content: Arc::from(content),
        color: msg
            .name_color
            .map(|color| format_color_rgb(color.r, color.g, color.b)),
        badges: msg
            .badges
            .iter()
            .map(|badge| format_badge(&badge.name, &badge.version))
            .collect(),
        timestamp,
        slots: slots.into(),
        structure_hash,
        reply: None,
        flags: crate::backend::models::MessageFlags {
            is_action,
            highlight_type,
            is_usernotice: true,
            ..Default::default()
        },
        twitch_gifs_tag: None,
        twitch_emotes_tag: msg
            .source
            .tags
            .0
            .get("emotes")
            .map(|tag| Arc::from(tag.as_str())),
        user_id: Some(Arc::from(msg.sender.id.as_str())),
        reply_emote_offset: action_prefix_chars,
    }
}

pub(super) fn build_usernotice_system_row(
    msg: &twitch_irc::message::UserNoticeMessage,
    text: &str,
    display_name: &str,
    id: String,
    timestamp: i64,
    highlight_type: Option<HighlightType>,
) -> ChatMessage {
    let recipient_display_name = msg
        .source
        .tags
        .0
        .get("msg-param-recipient-display-name")
        .filter(|name| !name.is_empty())
        .or_else(|| {
            msg.source
                .tags
                .0
                .get("msg-param-recipient-user-name")
                .filter(|name| !name.is_empty())
        });
    let mut names = Vec::with_capacity(2);
    if !display_name.is_empty() {
        names.push(display_name);
    }
    if let Some(recipient_display_name) = recipient_display_name {
        names.push(recipient_display_name.as_str());
    }
    let slots = usernotice_core::usernotice_system_slots(text, &names);
    let structure_hash = compute_structure_hash(&slots);
    ChatMessage {
        id: Arc::from(id),
        channel: Arc::from(msg.channel_login.as_str()),
        emote_lookup_channel: None,
        emote_lookup_source_id: None,
        sender: Arc::from(msg.sender.login.as_str()),
        display_name: Arc::from(display_name),
        content: Arc::from(text),
        color: msg
            .name_color
            .map(|color| format_color_rgb(color.r, color.g, color.b)),
        badges: smallvec::SmallVec::new(),
        timestamp,
        slots: slots.into(),
        structure_hash,
        reply: None,
        flags: crate::backend::models::MessageFlags {
            highlight_type,
            is_system: true,
            is_usernotice: true,
            system_type: Some(SystemMessageType::Info),
            ..Default::default()
        },
        twitch_gifs_tag: None,
        twitch_emotes_tag: None,
        user_id: Some(Arc::from(msg.sender.id.as_str())),
        reply_emote_offset: 0,
    }
}

pub(super) fn usernotice_highlight_type(kind: UsernoticeHighlightKind) -> HighlightType {
    match kind {
        UsernoticeHighlightKind::Subscription => HighlightType::Subscription,
        UsernoticeHighlightKind::Raid => HighlightType::Raid,
        UsernoticeHighlightKind::Announcement => HighlightType::Announcement { color: None },
        UsernoticeHighlightKind::BitsBadge => HighlightType::BitsBadge,
        UsernoticeHighlightKind::WatchStreak => HighlightType::WatchStreak,
    }
}
