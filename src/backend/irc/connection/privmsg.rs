//! PRIVMSG ingestion.

use super::*;

pub(super) fn reply_thread_has_current_user_participation(
    channel: &str,
    parent_msg_id: &str,
    username: &str,
) -> bool {
    if username.is_empty() || parent_msg_id.trim().is_empty() {
        return false;
    }

    let Some(buffer) = CHAT_MESSAGES.get(channel) else {
        return false;
    };

    buffer
        .iter_recent(PARTICIPATED_REPLY_SCAN_LIMIT)
        .any(|message| {
            if !message.sender.as_ref().eq_ignore_ascii_case(username) {
                return false;
            }

            message.id.as_ref() == parent_msg_id
                || message
                    .reply
                    .as_ref()
                    .map(|reply| reply.parent_msg_id.as_ref() == parent_msg_id)
                    .unwrap_or(false)
        })
}

pub(super) fn privmsg_tag<'a>(
    msg: &'a twitch_irc::message::PrivmsgMessage,
    key: &str,
) -> Option<&'a str> {
    msg.source.tags.0.get(key).map(String::as_str)
}

pub(super) fn privmsg_tag_owned(
    msg: &twitch_irc::message::PrivmsgMessage,
    key: &str,
) -> Option<String> {
    privmsg_tag(msg, key).map(str::to_string)
}

/// Removes the `@parent ` mention a reply's text starts with and returns how
/// many chars were removed, which is the shift the emotes tag needs. Only the
/// parent's own name is removed: a reply that opens by mentioning someone else
/// keeps that mention. Text that is only the mention is kept as is.
pub(crate) fn strip_reply_mention(
    text: &mut String,
    parent_login: &str,
    parent_display_name: Option<&str>,
) -> usize {
    let prefix_len = [Some(parent_login), parent_display_name]
        .into_iter()
        .flatten()
        .filter(|name| !name.is_empty())
        .find_map(|name| {
            let rest = text.strip_prefix('@')?;
            let mentioned = rest.get(..name.len())?;
            (mentioned.eq_ignore_ascii_case(name) && rest[name.len()..].starts_with(' '))
                .then_some(1 + name.len() + 1)
        });
    let Some(prefix_len) = prefix_len else {
        return 0;
    };
    if text.len() <= prefix_len {
        return 0;
    }
    let strip_chars = text[..prefix_len].chars().count();
    text.drain(..prefix_len);
    strip_chars
}

/// Process a PRIVMSG (chat message)
pub(super) async fn process_privmsg_message(msg: twitch_irc::message::PrivmsgMessage) {
    let channel_lower = msg.channel_login.to_lowercase();

    if !DESIRED_CHANNELS.read().contains(&channel_lower) {
        crate::log_irc!(
            debug,
            "Dropping late PRIVMSG for removed channel {}",
            channel_lower
        );
        return;
    }

    if should_hide_blocked_channel_message(
        Some(&msg.sender.id),
        &msg.sender.name,
        &msg.channel_login,
    ) {
        crate::log_irc!(
            debug,
            "Blocked users: Hiding message from {} in #{}",
            msg.sender.name,
            msg.channel_login
        );
        return;
    }

    if !admit_received_chat_message(&channel_lower, &msg.message_id) {
        crate::log_irc!(
            debug,
            "Skipping duplicate message {} in {}",
            msg.message_id,
            msg.channel_login
        );
        return;
    }

    {
        let channel_key = channel_lower.clone();
        let should_load_badges = BADGE_LOADED_CHANNELS.lock().insert(channel_key.clone());

        crate::backend::twitch_api::cache_channel_id(&msg.channel_login, &msg.channel_id);

        let should_load_emotes = EMOTE_LOADED_CHANNELS.lock().insert(channel_key);

        if should_load_badges || should_load_emotes {
            let channel_id = msg.channel_id.clone();
            let channel_login = msg.channel_login.clone();
            let load_badges = should_load_badges;
            let load_emotes = should_load_emotes;
            crate::backend::metrics::record_emote_stage(
                "irc.channel_assets.spawn",
                std::time::Duration::ZERO,
                1,
            );
            crate::log_perf!(
                debug,
                stage = "irc.channel_assets.spawn",
                channel = %channel_login,
                load_badges,
                load_emotes,
                "Spawning channel asset load"
            );

            tokio::spawn(async move {
                if load_badges
                    && let Err(e) = TWITCH_BADGE_CACHE
                        .load_channel_badges(&channel_id, &channel_login)
                        .await
                {
                    if e.is_proxy_not_configured() {
                        crate::log_cache!(
                            debug,
                            "Skipped channel badges for {}: {}",
                            channel_login,
                            e
                        );
                    } else {
                        crate::log_cache!(
                            warn,
                            "Failed to load channel badges for {}: {}",
                            channel_login,
                            e
                        );
                    }
                }
                if load_emotes {
                    load_or_defer_channel_emotes(channel_id, channel_login, "irc.first_message")
                        .await;
                }
            });
        }
    }

    // twitch-irc strips the CTCP ACTION framing from `message_text` and reports
    // it as `is_action`; a literal "/me " prefix is still treated as an action.
    let (mut clean_text, has_me_prefix) = normalize_message_text(&msg.message_text);
    let is_action = msg.is_action || has_me_prefix;

    let reply_parent_msg_id = privmsg_tag_owned(&msg, "reply-parent-msg-id");
    let reply_parent_user_login = privmsg_tag_owned(&msg, "reply-parent-user-login");

    let reply_mention_strip_len = reply_parent_user_login
        .as_deref()
        .map_or(0, |parent_login| {
            strip_reply_mention(
                &mut clean_text,
                parent_login,
                privmsg_tag(&msg, "reply-parent-display-name"),
            )
        });

    let username = get_username_lowercase();
    let contains_account_name = contains_account_mention(&clean_text, &username);
    let is_reply_to_current_user = !username.is_empty()
        && reply_parent_user_login
            .as_deref()
            .map(|parent_login| parent_login.eq_ignore_ascii_case(&username))
            .unwrap_or(false);
    let is_own_sender =
        !username.is_empty() && msg.sender.login.as_str().eq_ignore_ascii_case(&username);
    let is_reply_to_participated_thread = !is_own_sender
        && !is_reply_to_current_user
        && reply_parent_msg_id
            .as_deref()
            .map(|parent_id| {
                reply_thread_has_current_user_participation(&channel_lower, parent_id, &username)
            })
            .unwrap_or(false);
    let highlight_blacklisted_sender =
        crate::backend::settings::is_highlight_blacklisted_user(&msg.sender.login);
    // Your own messages never mention you.
    let mentions_current_user =
        !is_own_sender && (contains_account_name || is_reply_to_current_user);
    let highlighted = is_mention_highlight(highlight_blacklisted_sender, mentions_current_user);

    let twitch_emotes: FxHashMap<usize, (String, usize)> = msg
        .emotes
        .iter()
        .filter_map(|e| {
            if e.char_range.end <= reply_mention_strip_len {
                return None;
            }
            let new_start = e.char_range.start.saturating_sub(reply_mention_strip_len);
            let new_end = e.char_range.end.saturating_sub(reply_mention_strip_len);
            Some((new_start, (e.id.clone(), new_end)))
        })
        .collect();

    let source_room_id = privmsg_tag_owned(&msg, "source-room-id");
    let source_broadcaster_login =
        privmsg_tag(&msg, "source-broadcaster-user-login").map(str::to_lowercase);
    let lookup_selection = select_shared_chat_emote_lookup(
        &msg.channel_login,
        &msg.channel_id,
        source_broadcaster_login.as_deref(),
        source_room_id.as_deref(),
        crate::backend::twitch_api::get_cached_channel_login_by_id,
    );
    if source_broadcaster_login.is_some()
        && let Some(source_id) = source_room_id.as_deref()
    {
        crate::backend::twitch_api::cache_channel_id(&lookup_selection.channel_key, source_id);
    }
    let unresolved_source_room_id = lookup_selection.unresolved_source_room_id.clone();
    if let Some(source_id_owned) = lookup_selection.unresolved_source_room_id {
        crate::log_emotes!(
            debug,
            "[SHARED] Deferring source-room-id {} resolution for host channel {}",
            source_id_owned,
            msg.channel_login
        );
        if begin_source_room_lookup(&source_id_owned, &msg.channel_login, &msg.message_id) {
            tokio::spawn(async move {
                let resolved =
                    crate::backend::twitch_api::get_channel_login_by_id_cached(&source_id_owned)
                        .await;
                let waiters = finish_source_room_lookup(&source_id_owned, resolved.is_some());
                if let Some(resolved) = resolved {
                    crate::backend::twitch_api::cache_channel_id(&resolved, &source_id_owned);
                    for (host_channel, message_id) in waiters {
                        publish_resolved_message_emote_scope(&host_channel, &message_id, &resolved);
                    }
                    ensure_channel_emotes_loaded(&resolved, Some(&source_id_owned)).await;
                }
            });
        }
    }
    let emote_lookup_channel = lookup_selection.channel_key;

    if !emote_lookup_channel.is_empty() && emote_lookup_channel != msg.channel_login {
        let source_emote_channel = emote_lookup_channel.clone();
        let source_room_id = source_room_id.clone();
        tokio::spawn(async move {
            ensure_channel_emotes_loaded(&source_emote_channel, source_room_id.as_deref()).await;
        });
    }

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
    let replacement_applied = replaced_emote_tag.is_some();

    let twitch_gifs_tag: Option<Arc<str>> = (!replacement_applied)
        .then(|| privmsg_tag(&msg, "gifs").map(Arc::from))
        .flatten();
    let slots = crate::backend::slots::parser::parse_to_slots_with_gifs(
        &clean_text,
        &emote_lookup_channel,
        crate::backend::emotes::EmoteSender::new(Some(&msg.sender.id), &msg.sender.login),
        &twitch_emotes,
        twitch_gifs_tag.as_deref(),
        reply_mention_strip_len,
    );
    let structure_hash = compute_structure_hash(&slots);
    let slots: std::sync::Arc<[_]> = slots.into();

    let content = clean_text;

    let sender_login = msg.sender.login.clone();
    let display_name = msg.sender.name.clone();

    let color: Option<Arc<str>> = msg.name_color.map(|c| format_color_rgb(c.r, c.g, c.b));

    let is_first_msg = privmsg_tag(&msg, "first-msg") == Some("1");
    let is_returning_chatter = privmsg_tag(&msg, "returning-chatter") == Some("1");
    let is_elevated_message = privmsg_tag(&msg, "elevated-msg")
        .or_else(|| privmsg_tag(&msg, "elevated-message"))
        .or_else(|| privmsg_tag(&msg, "elevated"))
        .is_some_and(|v| matches!(v, "1" | "true" | "TRUE"));
    let is_hype_chat = msg
        .source
        .tags
        .0
        .keys()
        .any(|key| key.starts_with("pinned-chat-paid-"));

    let is_shared_chat = source_room_id
        .as_deref()
        .is_some_and(|source_id| source_id != msg.channel_id);

    let reply_parent_display_name = privmsg_tag_owned(&msg, "reply-parent-display-name");
    let reply_parent_user_id = privmsg_tag_owned(&msg, "reply-parent-user-id");
    let reply_parent_msg_body = privmsg_tag_owned(&msg, "reply-parent-msg-body");
    let is_reply = reply_parent_msg_id.is_some();

    let sub_length: Option<u32> = msg
        .badge_info
        .iter()
        .find(|b| b.name == "subscriber")
        .and_then(|b| b.version.parse().ok());
    let bits_amount = privmsg_tag(&msg, "bits")
        .and_then(|value| value.parse::<u32>().ok())
        .unwrap_or(0);
    let is_cheer = bits_amount > 0;
    let badge_keys_for_highlights: Vec<String> = msg
        .badges
        .iter()
        .map(|badge| format_badge(&badge.name, &badge.version).to_string())
        .collect();
    let reward_id = privmsg_tag_owned(&msg, "custom-reward-id").unwrap_or_default();
    let is_reward_message = !reward_id.is_empty();

    let timestamp = privmsg_tag(&msg, "tmi-sent-ts")
        .and_then(|ts| ts.parse::<i64>().ok())
        .unwrap_or_else(|| Utc::now().timestamp_millis());

    let similarity_settings = get_similarity_settings();
    let is_similar = CHAT_MESSAGES
        .get(&msg.channel_login)
        .and_then(|buffer| {
            detect_similarity(
                &similarity_settings,
                &content,
                &sender_login,
                timestamp,
                buffer
                    .iter_recent(similarity_settings.max_messages_to_check as usize)
                    .map(|msg| SimilarityCandidate {
                        content: &msg.content,
                        sender: &msg.sender,
                        timestamp: msg.timestamp,
                    }),
            )
        })
        .is_some();

    let filter_ctx = TestContext {
        content: content.clone(),
        author: sender_login.clone(),
        author_login: sender_login.clone(),
        author_user_id: msg.sender.id.clone(),
        author_color: color.as_ref().map(|c| c.to_string()),
        badges: badge_names_for_filter(msg.badges.iter().map(|b| b.name.as_str())),
        channel_name: msg.channel_login.clone(),
        bits_amount,
        reward_id: reward_id.clone(),
        sub_length,
        is_sub: msg.badges.iter().any(|b| b.name == "subscriber"),
        is_first_msg,
        is_returning_chatter,
        is_action,
        is_reply,
        is_whisper: false,
        is_announcement: false,
        is_system: false,
        is_highlighted: highlighted,
        is_cheer,
        is_shared_chat,
        is_similar,
        is_points_redeemed: is_reward_message,
        is_reward_message,
        is_elevated_message,
        is_hype_chat,
        channel_watching: true,
        channel_live: crate::backend::bridge::get_cached_channel_live_status(&msg.channel_login),
        ..Default::default()
    };

    let exclude_self_from_filters =
        crate::backend::settings::get_exclude_user_messages_from_filter()
            && sender_login.eq_ignore_ascii_case(&get_username_lowercase());
    let filter_results = if exclude_self_from_filters {
        Vec::new()
    } else {
        FILTER_STORE.test_message(&filter_ctx)
    };

    let PrivmsgFilterOutcome {
        hide: should_hide,
        highlighted: filter_highlighted,
        matched_text: filter_highlight_match,
    } = fold_privmsg_filter_results(&filter_results);

    if !should_hide && is_similar && similarity_settings.hide_similar {
        crate::log_irc!(debug, "Similarity: Hiding message from {}", sender_login);
        return;
    }

    if should_hide {
        crate::log_irc!(debug, "Filter: Hiding message from {}", sender_login);
        return;
    }

    let matched_configured_highlight = if highlight_blacklisted_sender {
        None
    } else {
        crate::backend::settings::match_highlight_rule_for_targets(
            &content,
            &sender_login,
            &badge_keys_for_highlights,
            &[
                crate::backend::settings::HighlightRuleTarget::Phrase,
                crate::backend::settings::HighlightRuleTarget::User,
            ],
        )
    };
    let matched_configured_badge_highlight = if highlight_blacklisted_sender {
        None
    } else {
        crate::backend::settings::match_highlight_rule_for_targets(
            &content,
            &sender_login,
            &badge_keys_for_highlights,
            &[crate::backend::settings::HighlightRuleTarget::Badge],
        )
    };

    let mut highlight_decision = decide_privmsg_highlight(
        PrivmsgHighlightSignals {
            blacklisted_sender: highlight_blacklisted_sender,
            account_name_mention: mentions_current_user,
            own_sender: is_own_sender,
            participated_thread_username: is_reply_to_participated_thread
                .then_some(username.as_str()),
            filter_matched_text: filter_highlight_match,
            filter_highlighted,
            cheer: is_cheer,
            first_message: is_first_msg,
            returning_chatter: is_returning_chatter,
            elevated: is_elevated_message || is_hype_chat,
            similar_dim_candidate: is_similar && similarity_settings.color_similar,
        },
        matched_configured_highlight.as_ref(),
        matched_configured_badge_highlight.as_ref(),
        || {
            crate::backend::settings::match_highlight_rule_effects_for_targets(
                &content,
                &sender_login,
                &badge_keys_for_highlights,
                &[
                    crate::backend::settings::HighlightRuleTarget::Phrase,
                    crate::backend::settings::HighlightRuleTarget::User,
                    crate::backend::settings::HighlightRuleTarget::Badge,
                ],
            )
        },
    );
    let highlight_type = highlight_decision.highlight_type.take();
    let is_similar_dim = highlight_decision.is_similar_dim;

    let reply = match (reply_parent_msg_id, reply_parent_user_login) {
        (Some(parent_id), Some(parent_user)) => Some(ReplyInfo {
            parent_msg_id: Arc::from(parent_id),
            parent_user_login: Arc::from(parent_user.clone()),
            parent_display_name: Arc::from(reply_parent_display_name.unwrap_or(parent_user)),
            parent_user_id: reply_parent_user_id.map(Arc::from),
            parent_msg_body: reply_parent_msg_body.map(Arc::from),
        }),
        _ => None,
    };

    let twitch_emotes_tag = privmsg_tag_owned(&msg, "emotes");

    let mut chat_msg = ChatMessage {
        id: Arc::from(msg.message_id.as_str()),
        channel: Arc::from(msg.channel_login.as_str()),
        emote_lookup_channel: (emote_lookup_channel != msg.channel_login)
            .then(|| Arc::from(emote_lookup_channel.as_str())),
        emote_lookup_source_id: unresolved_source_room_id.map(Arc::from),
        sender: Arc::from(sender_login.as_str()),
        display_name: Arc::from(display_name.as_str()),
        content: content.into(),
        color: color.clone(),
        badges: msg
            .badges
            .iter()
            .map(|b| format_badge(&b.name, &b.version))
            .collect(),
        timestamp,
        slots,
        structure_hash,
        reply,
        flags: crate::backend::models::MessageFlags {
            is_action,
            highlight_type,
            is_first_message: is_first_msg,
            is_shared_chat,
            is_elevated_message,
            is_hype_chat,
            is_similar: is_similar_dim,
            ..Default::default()
        },
        twitch_gifs_tag,
        twitch_emotes_tag: match replaced_emote_tag {
            Some(tag) => tag.map(|t| Arc::from(t.as_str())),
            None => twitch_emotes_tag.map(|t| Arc::from(t.as_str())),
        },
        user_id: Some(Arc::from(msg.sender.id.as_str())),
        reply_emote_offset: if replacement_applied {
            0
        } else {
            reply_mention_strip_len
        },
    };

    let visibility_commit = crate::backend::blocked_users::visibility_commit_guard();
    if !DESIRED_CHANNELS.read().contains(&channel_lower) {
        crate::log_irc!(
            debug,
            "Dropping late PRIVMSG after final visibility fence for removed channel {}",
            channel_lower
        );
        return;
    }
    let blocked_visibility = blocked_channel_visibility_snapshot(&msg.channel_login);
    if blocked_visibility.should_hide_sender(Some(&msg.sender.id), &sender_login) {
        crate::log_irc!(
            debug,
            "Blocked users: Hiding message from {} after final visibility recheck in #{}",
            sender_login,
            msg.channel_login
        );
        return;
    }
    if chat_msg.reply.as_ref().is_some_and(|reply| {
        blocked_visibility.should_redact_reply_parent(reply.parent_user_login.as_ref())
    }) {
        chat_msg.reply = None;
    }

    let initial_visibility_pending = crate::backend::blocked_users::initial_visibility_pending();
    if !initial_visibility_pending {
        crate::log_irc!(debug, "Received: {}: {}", msg.sender.name, msg.message_text);
    } else if !pending_channel_row_capacity_available_commit_held(&visibility_commit) {
        drop(visibility_commit);
        return;
    }

    let replaced_confirmed_local_row = {
        let mut buffer = CHAT_MESSAGES
            .entry(msg.channel_login.clone())
            .or_insert_with(MessageBuffer::with_default_capacity);
        store_received_chat_row(&mut buffer, &mut chat_msg)
    };
    apply_pending_message_emote_scope(&msg.channel_login, chat_msg.id.as_ref());
    if replaced_confirmed_local_row {
        let affected_ids = std::iter::once(chat_msg.id.clone()).collect::<HashSet<_>>();
        crate::components::chat::canvas_renderer::global_layout_cache::clear_layouts_for_messages(
            &affected_ids,
        );
        bump_chat_version();
        crate::backend::history_cache::record_live_chat_message(&msg.channel_login, &chat_msg);
        crate::backend::observers::get_observer_registry().notify(
            &msg.channel_login,
            crate::backend::observers::ChannelEvent::MessagesRetokenized,
        );
        drop(visibility_commit);
        return;
    }
    if initial_visibility_pending {
        defer_buffered_row_visibility_commit_held(
            &visibility_commit,
            &msg.channel_login,
            chat_msg.id.as_ref(),
        );
        drop(visibility_commit);
        return;
    }

    crate::bench::live_channel::note_inbound_privmsg(&msg.channel_login);

    if let Some(ref color_str) = color {
        cache_user_color(&sender_login, color_str);
    }
    track_chatter(
        &msg.channel_login,
        &sender_login,
        &display_name,
        color.as_ref().map(|color| color.to_string()),
    );

    bump_chat_version();
    crate::backend::history_cache::record_live_chat_message(&msg.channel_login, &chat_msg);

    let is_mention_activity = privmsg_shows_in_mentions(
        highlighted,
        chat_msg.flags.highlight_type.as_ref(),
        matched_configured_highlight.as_ref(),
        matched_configured_badge_highlight.as_ref(),
    );

    let mut tab_activity = TAB_ACTIVITY_MESSAGE;
    if is_mention_activity {
        tab_activity |= TAB_ACTIVITY_MENTION;
    }
    mark_channel_tab_activity(&msg.channel_login, tab_activity);

    if is_mention_activity {
        let evicted = {
            let mut mentions = GLOBAL_MENTIONS.write();
            let evicted = (mentions.len() >= MAX_ACTIVITY_MENTIONS)
                .then(|| mentions.pop_front())
                .flatten();
            mentions.push_back((msg.channel_login.clone(), chat_msg.clone()));
            evicted
        };
        if let Some((evicted_channel, evicted_message)) = evicted {
            remove_global_mention_unread(&evicted_channel, evicted_message.id.as_ref());
        }
        mark_global_mention_unread(&msg.channel_login, chat_msg.id.as_ref());
        crate::state::ui_invalidation::request(
            crate::state::ui_invalidation::UiInvalidation::TopBarState,
        );
    }

    let slots_for_preload = Arc::clone(&chat_msg.slots);
    let msg_id_for_preload = Arc::clone(&chat_msg.id);
    let preload_emote_channel = emote_lookup_channel.clone();

    crate::backend::observers::get_observer_registry().notify(
        &msg.channel_login,
        crate::backend::observers::ChannelEvent::NewMessage(chat_msg.clone()),
    );
    drop(visibility_commit);

    match highlight_decision.sound() {
        Some(PrivmsgHighlightSound::Event(sound)) => {
            SOUND_STORE.play_sound_for_channel(sound, &msg.channel_login);
        }
        Some(PrivmsgHighlightSound::RuleFile(sound_path)) => {
            SOUND_STORE.play_custom_sound_for_channel(
                sound_path,
                SoundEventType::Phrase,
                &msg.channel_login,
            );
        }
        None => {}
    }
    match highlight_decision.alert() {
        PrivmsgHighlightAlert::Mention => {
            if crate::backend::settings::get_flash_on_mention() {
                crate::backend::notifications::flash_taskbar_for_attention();
            }
            notify_mention(MentionNotification {
                channel: msg.channel_login.clone(),
                author: chat_msg.display_name.to_string(),
                message: chat_msg.content.to_string(),
            });
        }
        PrivmsgHighlightAlert::Rule => {
            crate::backend::notifications::flash_taskbar_for_attention();
            notify_highlight(
                HighlightNotification {
                    channel: msg.channel_login.clone(),
                    author: chat_msg.display_name.to_string(),
                    message: chat_msg.content.to_string(),
                },
                SoundEventType::Phrase,
            );
        }
        PrivmsgHighlightAlert::None => {}
    }

    preload_message_content(
        &slots_for_preload,
        &preload_emote_channel,
        msg_id_for_preload.as_ref(),
    );
}

pub(crate) fn badge_names_for_filter<'a>(names: impl IntoIterator<Item = &'a str>) -> Vec<String> {
    let mut badges: Vec<String> = Vec::new();
    let mut has_moderator = false;
    let mut has_lead_moderator = false;

    for name in names {
        if name.is_empty() {
            continue;
        }
        if crate::backend::badges::badge_name_matches(name, "moderator") {
            has_moderator = true;
        }
        if crate::backend::badges::is_lead_moderator_badge_name(name) {
            has_lead_moderator = true;
        }
        if !badges
            .iter()
            .any(|badge| crate::backend::badges::badge_name_matches(badge.as_str(), name))
        {
            badges.push(name.to_string());
        }
    }

    if has_lead_moderator && !has_moderator {
        badges.push("moderator".to_string());
    }

    badges
}

#[cfg(test)]
mod tests {
    use super::strip_reply_mention;
    use core::prelude::v1::test;

    #[test]
    fn strips_only_the_parent_mention_from_a_reply() {
        let mut text = "@Alice thanks Kappa".to_string();
        assert_eq!(strip_reply_mention(&mut text, "alice", None), 7);
        assert_eq!(text, "thanks Kappa");

        let mut text = "@bob look at this".to_string();
        assert_eq!(strip_reply_mention(&mut text, "alice", Some("Alice")), 0);
        assert_eq!(text, "@bob look at this");

        let mut text = "@アリス はい".to_string();
        assert_eq!(strip_reply_mention(&mut text, "alice", Some("アリス")), 5);
        assert_eq!(text, "はい");

        let mut text = "@alice ".to_string();
        assert_eq!(strip_reply_mention(&mut text, "alice", None), 0);
        assert_eq!(text, "@alice ");

        let mut text = "@alicia hi".to_string();
        assert_eq!(strip_reply_mention(&mut text, "alice", None), 0);
    }
}
