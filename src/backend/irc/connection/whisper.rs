//! Incoming WHISPER ingestion.

use super::*;

/// Process a whisper message
pub(super) async fn process_whisper_message(msg: twitch_irc::message::WhisperMessage) {
    if crate::backend::blocked_users::should_hide_whisper(Some(&msg.sender.id), &msg.sender.name) {
        crate::log_irc!(
            debug,
            "Blocked users: Hiding whisper from {}",
            msg.sender.name
        );
        return;
    }

    let (clean_text, is_action) = normalize_message_text(&msg.message_text);
    let color = msg.name_color.map(|c| format_color_rgb(c.r, c.g, c.b));

    let filter_ctx = TestContext {
        content: clean_text.clone(),
        author: msg.sender.name.clone(),
        author_login: msg.sender.name.clone(),
        author_user_id: msg.sender.id.clone(),
        author_color: color.as_ref().map(|c| c.to_string()),
        badges: badge_names_for_filter(msg.badges.iter().map(|b| b.name.as_str())),
        is_action,
        is_whisper: true,
        is_highlighted: true,
        channel_watching: true,
        ..Default::default()
    };

    let mut should_hide = false;
    for result in FILTER_STORE.test_message(&filter_ctx) {
        if !result.matched {
            continue;
        }

        match result.action {
            FilterAction::Hide => {
                should_hide = true;
                break;
            }
            FilterAction::Allow => {
                should_hide = false;
                break;
            }
            FilterAction::Highlight => {}
            FilterAction::Replace => {}
        }
    }

    if should_hide {
        crate::log_irc!(debug, "Filter: Hiding whisper from {}", msg.sender.name);
        return;
    }

    let twitch_emotes: FxHashMap<usize, (String, usize)> = msg
        .emotes
        .iter()
        .map(|e| (e.char_range.start, (e.id.clone(), e.char_range.end)))
        .collect();

    let (clean_text, twitch_emotes, replaced_emote_tag) =
        match crate::backend::filters_replace::apply_ignore_replacements(
            &clean_text,
            &twitch_emotes,
        ) {
            Some((new_text, new_map)) => {
                let tag = crate::backend::filters_replace::serialize_emote_map(&new_map);
                (new_text, new_map, Some(tag))
            }
            None => (clean_text, twitch_emotes, None),
        };

    let slots: Vec<_> = parse_to_slots(
        &clean_text,
        crate::backend::whispers::WHISPERS_CHANNEL,
        crate::backend::emotes::EmoteSender::new(Some(&msg.sender.id), &msg.sender.login),
        &twitch_emotes,
    );
    let structure_hash = compute_structure_hash(&slots);
    let slots: std::sync::Arc<[_]> = slots.into();

    let twitch_emotes_tag = msg.source.tags.0.get("emotes").cloned();

    let timestamp = msg
        .source
        .tags
        .0
        .get("tmi-sent-ts")
        .and_then(|ts| ts.parse::<i64>().ok())
        .unwrap_or_else(|| Utc::now().timestamp_millis());

    let chat_msg = ChatMessage {
        id: Uuid::new_v4().to_string().into(),
        channel: Arc::from(crate::backend::whispers::WHISPERS_CHANNEL),
        emote_lookup_channel: None,
        emote_lookup_source_id: None,
        sender: Arc::from(msg.sender.name.as_str()),
        display_name: Arc::from(msg.sender.name.as_str()),
        content: clean_text.into(),
        color,
        badges: msg
            .badges
            .iter()
            .map(|b| format_badge(&b.name, &b.version))
            .collect(),
        timestamp,
        slots,
        structure_hash,
        reply: None,
        flags: crate::backend::models::MessageFlags {
            is_action,
            highlight_type: Some(HighlightType::Whisper),
            ..Default::default()
        },
        twitch_gifs_tag: None,
        twitch_emotes_tag: match replaced_emote_tag {
            Some(tag) => tag.map(|t| Arc::from(t.as_str())),
            None => twitch_emotes_tag.map(|t| Arc::from(t.as_str())),
        },
        user_id: Some(Arc::from(msg.sender.id.as_str())),
        reply_emote_offset: 0,
    };

    let visibility_commit = crate::backend::blocked_users::visibility_commit_guard();
    if crate::backend::blocked_users::initial_visibility_pending() {
        let dropped_oldest = crate::backend::blocked_users::defer_whisper_visibility_commit_held(
            &visibility_commit,
            &msg.sender.id,
            &msg.sender.name,
            chat_msg.clone(),
        );
        crate::log_irc!(
            debug,
            "Blocked users: Deferred an incoming whisper until initial visibility becomes authoritative"
        );
        if dropped_oldest {
            crate::log_irc!(
                warn,
                "Blocked users: Pending whisper queue reached its bounded capacity; dropped the oldest deferred whisper"
            );
        }
        return;
    }
    if crate::backend::blocked_users::should_hide_whisper(Some(&msg.sender.id), &msg.sender.name) {
        crate::log_irc!(
            debug,
            "Blocked users: Hiding whisper from {} after final visibility recheck",
            msg.sender.name
        );
        return;
    }
    crate::log_irc!(
        debug,
        "Whisper from {}: {}",
        msg.sender.name,
        msg.message_text
    );
    let publication = crate::backend::whispers::record_received_visibility_commit_held(
        &visibility_commit,
        chat_msg.clone(),
    );
    drop(visibility_commit);
    crate::backend::whispers::dispatch_publication(publication);
    crate::state::ui_invalidation::request(
        crate::state::ui_invalidation::UiInvalidation::TopBarState,
    );
    SOUND_STORE.play_sound(SoundEventType::Whisper);
    notify_whisper(&chat_msg.display_name, &chat_msg.content);
}
