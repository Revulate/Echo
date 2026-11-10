//! Recent-message history for a channel: fetched from the recent-messages
//! service when a channel opens or reconnects, with the local history cache as
//! the fallback.

use super::irc::format::{CTCP_ACTION_PREFIX_CHARS, strip_ctcp_action};
use super::models::{ChatMessage, HighlightType, MessageFlags, SystemMessageType};
use super::slots::compute_structure_hash;
use super::slots::parser::{parse_emotes_tag_with_offset, parse_to_slots};
use backon::ExponentialBuilder;
use backon::Retryable;
use chrono::Utc;
use rustc_hash::FxHashMap;
use serde::Deserialize;
use std::time::Duration;
use uuid::Uuid;

/// Zonian merges recent-messages.robotty.de with logs instances and fails over
/// between them server-side.
const RECENT_MESSAGES_API: &str = "https://logs.zonian.dev/api/v2/recent-messages";
/// Used directly only when the Zonian host itself is unreachable or erroring.
const RECENT_MESSAGES_FALLBACK_API: &str =
    "https://recent-messages.robotty.de/api/v2/recent-messages";

/// Recent-message fetches gate reconnect and wake recovery, so a stalled host
/// must fail instead of pinning them.
static HISTORY_HTTP_CLIENT: std::sync::LazyLock<crate::backend::http::ClientCache> =
    std::sync::LazyLock::new(|| crate::backend::http::ClientCache::new(build_history_http_client));

fn build_history_http_client() -> reqwest::Client {
    crate::backend::http::client_builder()
        .timeout(Duration::from_secs(15))
        .connect_timeout(Duration::from_secs(5))
        .build()
        .unwrap_or_else(|error| {
            crate::log_net!(
                error,
                "Failed to create recent-messages HTTP client: {error}"
            );
            crate::backend::http::timed_client(Duration::from_secs(15))
        })
}
const DEFAULT_LIMIT: u32 = 500;
const HISTORY_EMOTE_PRELOAD_MESSAGE_LIMIT: usize = 80;
const HISTORY_EMOTE_PRELOAD_URL_LIMIT: usize = 96;

pub struct HistoryFetchResult {
    pub messages: Vec<ChatMessage>,
    pub cache_fallback_error: Option<String>,
}

/// Extract and preload emote URLs from message history
/// Background preloading ensures emotes are ready before messages enter viewport
fn preload_emotes_from_history(messages: &[ChatMessage], channel: &str) {
    use super::slots::MessageSlot;
    use rustc_hash::FxHashSet;
    use std::sync::Arc;

    crate::log_net!(
        info,
        "ENTRY: Processing {} messages for channel {}",
        messages.len(),
        channel
    );

    let messages_to_scan = messages.len().min(HISTORY_EMOTE_PRELOAD_MESSAGE_LIMIT);
    let estimated_emotes = (messages_to_scan * 3).min(HISTORY_EMOTE_PRELOAD_URL_LIMIT);
    let mut media_requests = Vec::with_capacity(estimated_emotes);
    let mut seen_urls: FxHashSet<Arc<str>> = FxHashSet::default();
    let mut seen_twitch_ids: FxHashSet<&str> = FxHashSet::default();

    'messages: for msg in messages.iter().rev().take(messages_to_scan) {
        for slot in msg.slots.iter() {
            match slot {
                MessageSlot::Emote(emote) => {
                    let url = super::emotes::chat_display_url_shared(emote);
                    if seen_urls.insert(Arc::clone(&url)) {
                        media_requests.push(crate::cache::loader::MediaRequest::new(url, None));
                        if media_requests.len() >= HISTORY_EMOTE_PRELOAD_URL_LIMIT {
                            break 'messages;
                        }
                    }
                }

                MessageSlot::TwitchEmote { id, .. } => {
                    if !seen_twitch_ids.insert(id.as_ref()) {
                        continue;
                    }
                    let url: Arc<str> = Arc::from(format!(
                        "https://static-cdn.jtvnw.net/emoticons/v2/{}/default/dark/{}",
                        id, "3.0"
                    ));
                    if seen_urls.insert(Arc::clone(&url)) {
                        media_requests.push(crate::cache::loader::MediaRequest::new(url, None));
                        if media_requests.len() >= HISTORY_EMOTE_PRELOAD_URL_LIMIT {
                            break 'messages;
                        }
                    }
                }

                _ => {}
            }
        }
    }

    if !media_requests.is_empty() {
        crate::log_net!(
            info,
            "Triggering lazy preload for {} unique emotes from newest {} of {} history messages in channel {}",
            media_requests.len(),
            messages_to_scan,
            messages.len(),
            channel
        );
        crate::cache::loader::trigger_lazy_preload_media(
            media_requests,
            crate::cache::loader::PreloadSource::History,
        );
    } else {
        crate::log_net!(
            info,
            "No emotes found in {} messages for channel {}",
            messages.len(),
            channel
        );
    }
}

#[derive(Debug, Deserialize)]
struct RecentMessagesResponse {
    #[serde(default)]
    messages: Vec<String>,
    #[serde(default)]
    error: Option<String>,
}

/// Fetch recent messages with default limit (500)
pub async fn fetch_recent_messages(channel: &str) -> Result<Vec<ChatMessage>, String> {
    fetch_recent_messages_with_limit(channel, DEFAULT_LIMIT).await
}

pub(crate) fn apply_blocked_visibility(
    mut messages: Vec<ChatMessage>,
    channel: &str,
) -> Vec<ChatMessage> {
    let visibility = crate::backend::irc::blocked_channel_visibility_snapshot(channel);
    messages.retain_mut(|message| {
        if super::irc::is_user_attributed_message(message)
            && visibility.should_hide_sender(message.user_id.as_deref(), message.sender.as_ref())
        {
            return false;
        }
        if message.reply.as_ref().is_some_and(|reply| {
            visibility.should_redact_reply_parent(reply.parent_user_login.as_ref())
        }) {
            message.reply = None;
        }
        true
    });
    messages
}

/// Most messages requested from the recent-messages service per channel.
const HISTORY_FETCH_LIMIT: u32 = 800;

/// Fetch a channel's recent messages in one request of up to
/// [`HISTORY_FETCH_LIMIT`], falling back to the local history cache when the
/// service is unavailable. Fetched messages are saved to that cache.
pub async fn fetch_recent_messages_streaming(channel: &str) -> Result<HistoryFetchResult, String> {
    match fetch_recent_messages_with_limit(channel, HISTORY_FETCH_LIMIT).await {
        Ok(messages) if !messages.is_empty() => {
            let count = messages.len();

            crate::log_net!(
                info,
                "Loaded {} history messages for {} (API fetch)",
                count,
                channel
            );

            let cache_channel = channel.to_string();
            let cache_messages = messages.clone();
            crate::backend::bridge::get_runtime().spawn_blocking(move || {
                super::history_cache::save_channel_history(&cache_channel, &cache_messages);
            });

            preload_emotes_from_history(&messages, channel);

            Ok(HistoryFetchResult {
                messages,
                cache_fallback_error: None,
            })
        }
        Ok(_empty) => {
            let cached = apply_blocked_visibility(
                super::history_cache::load_channel_history(channel),
                channel,
            );
            if cached.is_empty() {
                crate::log_net!(
                    info,
                    "No history for {} (API empty, no local cache)",
                    channel
                );
                Ok(HistoryFetchResult {
                    messages: vec![],
                    cache_fallback_error: None,
                })
            } else {
                let count = cached.len();
                crate::log_net!(
                    info,
                    "Loaded {} history messages for {} (local cache, API empty)",
                    count,
                    channel
                );
                preload_emotes_from_history(&cached, channel);
                Ok(HistoryFetchResult {
                    messages: cached,
                    cache_fallback_error: None,
                })
            }
        }
        Err(e) => {
            let cached = apply_blocked_visibility(
                super::history_cache::load_channel_history(channel),
                channel,
            );
            if cached.is_empty() {
                crate::log_net!(
                    warn,
                    "History API failed for {}: {} (no local cache fallback)",
                    channel,
                    e
                );
                Err(e)
            } else {
                let count = cached.len();
                crate::log_net!(
                    info,
                    "Loaded {} history messages for {} (local cache fallback, API error: {})",
                    count,
                    channel,
                    e
                );
                preload_emotes_from_history(&cached, channel);
                Ok(HistoryFetchResult {
                    messages: cached,
                    cache_fallback_error: Some(e),
                })
            }
        }
    }
}

/// Fetch recent messages with custom limit (for adaptive reconnect loading)
/// Limit is clamped to API max of 800
pub async fn fetch_recent_messages_with_limit(
    channel: &str,
    limit: u32,
) -> Result<Vec<ChatMessage>, String> {
    let clamped_limit = std::cmp::min(limit, 800);
    let raw_messages =
        match fetch_raw_recent_messages(RECENT_MESSAGES_API, channel, clamped_limit, 0).await {
            Ok(raw) => raw,
            Err(RecentMessagesError::Rejected(error)) => return Err(error),
            Err(RecentMessagesError::Unavailable(error)) => {
                crate::log_net!(
                    warn,
                    "Primary recent-message API failed for {}: {}; trying fallback",
                    channel,
                    error
                );
                match fetch_raw_recent_messages(
                    RECENT_MESSAGES_FALLBACK_API,
                    channel,
                    clamped_limit,
                    3,
                )
                .await
                {
                    Ok(raw) => raw,
                    Err(
                        RecentMessagesError::Rejected(error)
                        | RecentMessagesError::Unavailable(error),
                    ) => {
                        return Err(error);
                    }
                }
            }
        };

    let mut messages = Vec::with_capacity(raw_messages.len());

    for raw in raw_messages {
        match twitch_irc::message::IRCMessage::parse(&raw) {
            Ok(msg) => {
                if let Some(chat_msg) = convert_history_irc_message(msg, channel).await {
                    messages.push(chat_msg);
                }
            }
            Err(e) => {
                crate::log_net!(debug, "Failed to parse IRC message: {} (raw: {})", e, raw);
            }
        }
    }

    Ok(apply_blocked_visibility(messages, channel))
}

enum RecentMessagesError {
    /// The service answered and refused the channel (e.g. invalid login);
    /// another host would give the same answer.
    Rejected(String),
    /// Transport failure, 5xx/429, or an unparseable body.
    Unavailable(String),
}

async fn fetch_raw_recent_messages(
    base_url: &str,
    channel: &str,
    limit: u32,
    retries: usize,
) -> Result<Vec<String>, RecentMessagesError> {
    let url = format!("{base_url}/{channel}?limit={limit}");
    let client = HISTORY_HTTP_CLIENT.client();

    let attempt = std::sync::atomic::AtomicU32::new(0);
    let fetch = || async {
        let started = std::time::Instant::now();
        let result = client.get(&url).send().await;
        crate::backend::http::log_http_outcome(
            crate::backend::http::LogCategory::Net,
            "GET",
            &url,
            result.as_ref().map(reqwest::Response::status),
            started.elapsed(),
            attempt.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1,
        );
        result
    };

    let backoff = ExponentialBuilder::default()
        .with_min_delay(Duration::from_millis(100))
        .with_max_delay(Duration::from_secs(2))
        .with_max_times(retries);

    let response = fetch
        .retry(backoff)
        .await
        .map_err(|e| RecentMessagesError::Unavailable(format!("Failed to fetch history: {}", e)))?;

    // log_http_outcome already warned with the channel's request path.
    let status = response.status();
    let retryable_status = status.is_server_error() || status.as_u16() == 429;
    let data: RecentMessagesResponse = match response.json().await {
        Ok(data) => data,
        Err(_) if !status.is_success() => {
            return Err(RecentMessagesError::Unavailable(format!(
                "Recent-message API returned HTTP {}",
                status
            )));
        }
        Err(e) => {
            return Err(RecentMessagesError::Unavailable(format!(
                "Failed to parse response: {}",
                e
            )));
        }
    };

    match data.error {
        // A not-yet-joined channel can still carry messages buffered earlier.
        Some(error) if status.is_success() && !data.messages.is_empty() => {
            crate::log_net!(info, "History API note for channel {}: {}", channel, error);
        }
        Some(error) => {
            crate::log_net!(warn, "History API error for channel {}: {}", channel, error);
            let error = format!("Recent-message API error: {}", error);
            return Err(if retryable_status {
                RecentMessagesError::Unavailable(error)
            } else {
                RecentMessagesError::Rejected(error)
            });
        }
        None if !status.is_success() => {
            let error = format!("Recent-message API returned HTTP {}", status);
            return Err(if retryable_status {
                RecentMessagesError::Unavailable(error)
            } else {
                RecentMessagesError::Rejected(error)
            });
        }
        None => {}
    }

    Ok(data.messages)
}

/// Parse one raw IRC line into an Echo chat message for the provided channel.
///
/// This supports chat `PRIVMSG` plus common Twitch server event commands as
/// local system rows for `/fakemsg`.
pub async fn parse_raw_irc_message_for_channel(raw: &str, channel: &str) -> Option<ChatMessage> {
    let msg = twitch_irc::message::IRCMessage::parse(raw).ok()?;
    let message = convert_irc_message(msg, channel).await?;
    apply_blocked_visibility(vec![message], channel).pop()
}

async fn convert_history_irc_message(
    msg: twitch_irc::message::IRCMessage,
    channel: &str,
) -> Option<ChatMessage> {
    if super::history_filter::is_history_state_only_command(&msg.command) {
        return None;
    }

    convert_irc_message(msg, channel).await
}

async fn convert_irc_message(
    msg: twitch_irc::message::IRCMessage,
    channel: &str,
) -> Option<ChatMessage> {
    match msg.command.as_str() {
        "PRIVMSG" => {
            let display_name = msg.tags.0.get("display-name").cloned();

            let sender_login = match msg.prefix.as_ref() {
                Some(twitch_irc::message::IRCPrefix::Full { nick, .. }) => Some(nick.clone()),
                Some(twitch_irc::message::IRCPrefix::HostOnly { host }) => Some(host.clone()),
                _ => None,
            };

            let sender = display_name
                .or_else(|| sender_login.clone())
                .unwrap_or_else(|| "unknown".to_string());

            let content = msg.params.last().cloned().unwrap_or_default();

            let (mut clean_content, is_action) = match strip_ctcp_action(&content) {
                Some(text) => (text.to_string(), true),
                None => (content, false),
            };

            let reply_parent_msg_id = msg.tags.0.get("reply-parent-msg-id").cloned();
            let reply_parent_user_login = msg.tags.0.get("reply-parent-user-login").cloned();
            let reply_parent_display_name = msg.tags.0.get("reply-parent-display-name").cloned();
            let reply_parent_user_id = msg.tags.0.get("reply-parent-user-id").cloned();
            let reply_parent_msg_body = msg.tags.0.get("reply-parent-msg-body").cloned();

            let reply_mention_strip_len = if let Some(ref parent_user) = reply_parent_user_login {
                let mention_prefix = format!("@{} ", parent_user);
                if clean_content
                    .as_bytes()
                    .get(..mention_prefix.len())
                    .is_some_and(|head| head.eq_ignore_ascii_case(mention_prefix.as_bytes()))
                {
                    clean_content = clean_content[mention_prefix.len()..].to_string();
                    mention_prefix.len()
                } else {
                    0
                }
            } else {
                0
            };

            let reply = match (reply_parent_msg_id, reply_parent_user_login) {
                (Some(parent_id), Some(parent_user)) => Some(super::models::ReplyInfo {
                    parent_msg_id: std::sync::Arc::from(parent_id),
                    parent_user_login: std::sync::Arc::from(parent_user.clone()),
                    parent_display_name: std::sync::Arc::from(
                        reply_parent_display_name.unwrap_or(parent_user),
                    ),
                    parent_user_id: reply_parent_user_id.map(std::sync::Arc::from),
                    parent_msg_body: reply_parent_msg_body.map(std::sync::Arc::from),
                }),
                _ => None,
            };

            let color = msg.tags.0.get("color").cloned();

            let user_id = msg.tags.0.get("user-id").cloned();

            let badges: smallvec::SmallVec<[std::sync::Arc<str>; 4]> = msg
                .tags
                .0
                .get("badges")
                .map(|s| s.split(',').map(std::sync::Arc::from).collect())
                .unwrap_or_default();

            let timestamp = msg
                .tags
                .0
                .get("tmi-sent-ts")
                .and_then(|ts| ts.parse::<i64>().ok())
                .unwrap_or_else(|| Utc::now().timestamp_millis());

            let id = msg
                .tags
                .0
                .get("id")
                .cloned()
                .unwrap_or_else(|| Uuid::new_v4().to_string());

            let twitch_emotes: FxHashMap<usize, (String, usize)> = msg
                .tags
                .0
                .get("emotes")
                .map(|s| parse_emotes_tag_with_offset(s, reply_mention_strip_len))
                .unwrap_or_default();

            let (clean_content, twitch_emotes, replaced_emote_tag) =
                match crate::backend::filters_replace::apply_ignore_replacements(
                    &clean_content,
                    &twitch_emotes,
                ) {
                    Some((new_text, new_map)) => {
                        let tag = crate::backend::filters_replace::serialize_emote_map(&new_map);
                        (new_text, new_map, Some(tag))
                    }
                    None => (clean_content, twitch_emotes, None),
                };
            let replacement_applied = replaced_emote_tag.is_some();

            let source_room_id = tag_value(&msg, "source-room-id");
            let source_broadcaster_login = tag_value(&msg, "source-broadcaster-user-login");
            let host_room_id = tag_value(&msg, "room-id").unwrap_or_default();
            let lookup_selection = crate::backend::irc::select_shared_chat_emote_lookup(
                channel,
                &host_room_id,
                source_broadcaster_login.as_deref(),
                source_room_id.as_deref(),
                crate::backend::twitch_api::get_cached_channel_login_by_id,
            );
            if source_broadcaster_login.is_some()
                && let Some(source_id) = source_room_id.as_deref()
            {
                crate::backend::twitch_api::cache_channel_id(
                    &lookup_selection.channel_key,
                    source_id,
                );
            }
            let emote_lookup_channel = lookup_selection.channel_key;

            let twitch_gifs_tag: Option<std::sync::Arc<str>> = (!replacement_applied)
                .then(|| {
                    msg.tags
                        .0
                        .get("gifs")
                        .map(|tag| std::sync::Arc::from(tag.as_str()))
                })
                .flatten();
            let slots = crate::backend::slots::parser::parse_to_slots_with_gifs(
                &clean_content,
                &emote_lookup_channel,
                crate::backend::emotes::EmoteSender::new(
                    user_id.as_deref(),
                    sender_login.as_deref().unwrap_or_default(),
                ),
                &twitch_emotes,
                twitch_gifs_tag.as_deref(),
                reply_mention_strip_len,
            );
            let structure_hash = compute_structure_hash(&slots);
            let slots: std::sync::Arc<[_]> = slots.into();

            crate::log_net!(
                trace,
                "Message {}: parsed with {} emote slots",
                id.get(..8).unwrap_or(id.as_str()),
                slots
                    .iter()
                    .filter(|s| {
                        matches!(
                            s,
                            super::slots::MessageSlot::Emote(_)
                                | super::slots::MessageSlot::TwitchEmote { .. }
                        )
                    })
                    .count()
            );

            let display_name = msg
                .tags
                .0
                .get("display-name")
                .cloned()
                .unwrap_or_else(|| sender.clone());

            let twitch_emotes_tag = msg.tags.0.get("emotes").cloned();
            let is_elevated_message = msg
                .tags
                .0
                .get("elevated-msg")
                .or_else(|| msg.tags.0.get("elevated-message"))
                .or_else(|| msg.tags.0.get("elevated"))
                .map(|v| matches!(v.as_str(), "1" | "true" | "TRUE"))
                .unwrap_or(false);
            let is_hype_chat = msg
                .tags
                .0
                .keys()
                .any(|key| key.starts_with("pinned-chat-paid-"));

            let username = super::settings::get_username_lowercase();
            let is_mention = !username.is_empty()
                && !sender_login
                    .as_deref()
                    .is_some_and(|login| login.eq_ignore_ascii_case(&username))
                && super::models::contains_ascii_case_insensitive(&clean_content, &username);
            let highlight_type = if is_mention {
                Some(super::models::HighlightType::Mention)
            } else {
                None
            };

            Some(ChatMessage {
                id: id.into(),
                channel: std::sync::Arc::from(channel),
                emote_lookup_channel: (emote_lookup_channel != channel)
                    .then(|| std::sync::Arc::from(emote_lookup_channel)),
                emote_lookup_source_id: lookup_selection
                    .unresolved_source_room_id
                    .map(std::sync::Arc::from),
                sender: sender.into(),
                display_name: display_name.into(),
                content: clean_content.into(),
                color: color.map(|c| std::sync::Arc::from(c.as_str())),
                badges,
                timestamp,
                slots,
                structure_hash,
                reply,
                flags: super::models::MessageFlags {
                    is_action,
                    highlight_type,
                    is_first_message: tag_value(&msg, "first-msg").as_deref() == Some("1"),
                    is_shared_chat: source_broadcaster_login.is_some()
                        || source_room_id
                            .as_deref()
                            .is_some_and(|source_id| source_id != host_room_id),
                    is_elevated_message,
                    is_hype_chat,
                    ..Default::default()
                },
                twitch_gifs_tag,
                twitch_emotes_tag: match replaced_emote_tag {
                    Some(tag) => tag.map(|t| std::sync::Arc::from(t.as_str())),
                    None => twitch_emotes_tag.map(|t| std::sync::Arc::from(t.as_str())),
                },
                user_id: user_id.map(|id| std::sync::Arc::from(id.as_str())),
                reply_emote_offset: if replacement_applied {
                    0
                } else {
                    reply_mention_strip_len
                },
            })
        }
        "USERNOTICE" => convert_history_usernotice(&msg, channel),
        _ => convert_fake_system_event(&msg, channel),
    }
}

fn convert_history_usernotice(
    msg: &twitch_irc::message::IRCMessage,
    channel: &str,
) -> Option<ChatMessage> {
    let sender_login = tag_value(msg, "login")
        .filter(|login| !login.trim().is_empty())
        .or_else(|| irc_sender_login(msg));
    let sender_login = sender_login.unwrap_or_else(|| "unknown".to_string());
    let display_name = tag_value(msg, "display-name").unwrap_or_else(|| sender_login.clone());
    let user_id = tag_value(msg, "user-id");
    let event = crate::backend::fakemsg_core::fake_irc_system_event(msg, channel)?;
    let highlight_type = fake_highlight_to_model(event.highlight);
    let base_id = tag_value(msg, "id").unwrap_or_else(|| Uuid::new_v4().to_string());
    let timestamp = tag_value(msg, "tmi-sent-ts")
        .and_then(|ts| ts.parse::<i64>().ok())
        .unwrap_or_else(|| Utc::now().timestamp_millis());

    if let Some(raw_content) = msg
        .params
        .get(1)
        .filter(|content| !content.trim().is_empty())
    {
        let (content, is_action, action_offset) = if let Some(text) = strip_ctcp_action(raw_content)
        {
            (text.to_string(), true, CTCP_ACTION_PREFIX_CHARS)
        } else {
            (raw_content.clone(), false, 0)
        };
        let twitch_emotes = tag_value(msg, "emotes")
            .map(|tag| parse_emotes_tag_with_offset(&tag, action_offset))
            .unwrap_or_default();
        let slots: Vec<_> = parse_to_slots(
            &content,
            channel,
            crate::backend::emotes::EmoteSender::new(user_id.as_deref(), &sender_login),
            &twitch_emotes,
        );
        let structure_hash = compute_structure_hash(&slots);
        let badges = tag_value(msg, "badges")
            .map(|badges| badges.split(',').map(std::sync::Arc::from).collect())
            .unwrap_or_default();

        return Some(ChatMessage {
            id: format!("{base_id}:content").into(),
            channel: channel.into(),
            emote_lookup_channel: None,
            emote_lookup_source_id: None,
            sender: sender_login.into(),
            display_name: display_name.into(),
            content: content.into(),
            color: tag_value(msg, "color")
                .filter(|color| !color.trim().is_empty())
                .map(Into::into),
            badges,
            timestamp,
            slots: slots.into(),
            structure_hash,
            reply: None,
            flags: MessageFlags {
                is_action,
                highlight_type,
                is_usernotice: true,
                ..Default::default()
            },
            twitch_gifs_tag: None,
            twitch_emotes_tag: tag_value(msg, "emotes").map(Into::into),
            user_id: user_id.map(Into::into),
            reply_emote_offset: action_offset,
        });
    }

    let recipient_display_name = tag_value(msg, "msg-param-recipient-display-name")
        .filter(|name| !name.trim().is_empty())
        .or_else(|| {
            tag_value(msg, "msg-param-recipient-user-name").filter(|name| !name.trim().is_empty())
        });
    let mut names = Vec::with_capacity(2);
    if !display_name.is_empty() {
        names.push(display_name.as_str());
    }
    if let Some(recipient_display_name) = recipient_display_name.as_deref() {
        names.push(recipient_display_name);
    }
    let slots = crate::backend::usernotice_core::usernotice_system_slots(&event.text, &names);

    let mut message = system_chat_message(channel, &event.text, highlight_type, msg);
    message.id = base_id.into();
    message.sender = sender_login.into();
    message.display_name = display_name.into();
    message.user_id = user_id.map(Into::into);
    message.color = tag_value(msg, "color")
        .filter(|color| !color.trim().is_empty())
        .map(Into::into);
    message.structure_hash = compute_structure_hash(&slots);
    message.slots = slots.into();
    message.flags.is_usernotice = true;
    Some(message)
}

fn irc_sender_login(msg: &twitch_irc::message::IRCMessage) -> Option<String> {
    match msg.prefix.as_ref() {
        Some(twitch_irc::message::IRCPrefix::Full { nick, .. }) => Some(nick.clone()),
        Some(twitch_irc::message::IRCPrefix::HostOnly { host }) => Some(host.clone()),
        _ => None,
    }
}

fn convert_fake_system_event(
    msg: &twitch_irc::message::IRCMessage,
    channel: &str,
) -> Option<ChatMessage> {
    let event = crate::backend::fakemsg_core::fake_irc_system_event(msg, channel)?;
    Some(system_chat_message(
        channel,
        &event.text,
        fake_highlight_to_model(event.highlight),
        msg,
    ))
}

fn fake_highlight_to_model(
    highlight: crate::backend::fakemsg_core::FakeIrcHighlight,
) -> Option<HighlightType> {
    match highlight {
        crate::backend::fakemsg_core::FakeIrcHighlight::None => None,
        crate::backend::fakemsg_core::FakeIrcHighlight::Whisper => Some(HighlightType::Whisper),
        crate::backend::fakemsg_core::FakeIrcHighlight::Subscription => {
            Some(HighlightType::Subscription)
        }
        crate::backend::fakemsg_core::FakeIrcHighlight::Raid => Some(HighlightType::Raid),
        crate::backend::fakemsg_core::FakeIrcHighlight::BitsBadge => Some(HighlightType::BitsBadge),
        crate::backend::fakemsg_core::FakeIrcHighlight::Announcement => {
            Some(HighlightType::Announcement { color: None })
        }
        crate::backend::fakemsg_core::FakeIrcHighlight::WatchStreak => {
            Some(HighlightType::WatchStreak)
        }
    }
}

fn system_chat_message(
    channel: &str,
    text: &str,
    highlight_type: Option<HighlightType>,
    raw: &twitch_irc::message::IRCMessage,
) -> ChatMessage {
    let slots: Vec<_> = parse_to_slots(
        text,
        channel,
        crate::backend::emotes::EmoteSender::NONE,
        &FxHashMap::default(),
    );
    let structure_hash = compute_structure_hash(&slots);
    let slots: std::sync::Arc<[_]> = slots.into();

    ChatMessage {
        id: tag_value(raw, "id")
            .or_else(|| tag_value(raw, "target-msg-id"))
            .unwrap_or_else(|| Uuid::new_v4().to_string())
            .into(),
        channel: std::sync::Arc::from(channel),
        emote_lookup_channel: None,
        emote_lookup_source_id: None,
        sender: std::sync::Arc::from("System"),
        display_name: std::sync::Arc::from("System"),
        content: std::sync::Arc::from(text),
        color: None,
        badges: smallvec::SmallVec::new(),
        timestamp: tag_value(raw, "tmi-sent-ts")
            .and_then(|ts| ts.parse::<i64>().ok())
            .unwrap_or_else(|| Utc::now().timestamp_millis()),
        slots,
        structure_hash,
        reply: None,
        flags: MessageFlags {
            highlight_type,
            is_system: true,
            is_usernotice: raw.command == "USERNOTICE",
            system_type: Some(SystemMessageType::Info),
            ..Default::default()
        },
        twitch_gifs_tag: None,
        twitch_emotes_tag: None,
        user_id: None,
        reply_emote_offset: 0,
    }
}

fn tag_value(msg: &twitch_irc::message::IRCMessage, key: &str) -> Option<String> {
    msg.tags.0.get(key).cloned()
}
