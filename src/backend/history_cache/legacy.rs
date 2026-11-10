//! Decoders for the history-cache formats written before V5; each converts to
//! the current [`StoredMessage`].

use super::*;

/// Frozen positional Postcard V4 DTO; keep its field order unchanged.
#[derive(Serialize, Deserialize, Clone)]
pub(super) struct StoredMessageV4 {
    pub(super) id: String,
    pub(super) sender: String,
    pub(super) display_name: String,
    pub(super) content: String,
    pub(super) emote_lookup_channel: Option<String>,
    pub(super) emote_lookup_source_id: Option<String>,
    pub(super) color: Option<String>,
    pub(super) badges: Vec<String>,
    pub(super) timestamp: i64,
    pub(super) user_id: Option<String>,
    pub(super) twitch_emotes_tag: Option<String>,
    pub(super) reply_emote_offset: usize,
    pub(super) is_action: bool,
    pub(super) reply_parent_msg_id: Option<String>,
    pub(super) reply_parent_user_login: Option<String>,
    pub(super) reply_parent_display_name: Option<String>,
    pub(super) reply_parent_msg_body: Option<String>,
    pub(super) reply_parent_user_id: Option<String>,
}

impl From<StoredMessageV4> for StoredMessage {
    fn from(value: StoredMessageV4) -> Self {
        Self {
            id: value.id,
            sender: value.sender,
            display_name: value.display_name,
            content: value.content,
            emote_lookup_channel: value.emote_lookup_channel,
            emote_lookup_source_id: value.emote_lookup_source_id,
            color: value.color,
            badges: value.badges,
            timestamp: value.timestamp,
            user_id: value.user_id,
            twitch_emotes_tag: value.twitch_emotes_tag,
            reply_emote_offset: value.reply_emote_offset,
            is_action: value.is_action,
            reply_parent_msg_id: value.reply_parent_msg_id,
            reply_parent_user_login: value.reply_parent_user_login,
            reply_parent_display_name: value.reply_parent_display_name,
            reply_parent_msg_body: value.reply_parent_msg_body,
            reply_parent_user_id: value.reply_parent_user_id,
            twitch_gifs_tag: None,
        }
    }
}

/// Exact DTO written with `HISTORY_CACHE_V3_MAGIC`.
///
/// Keep this field order stable: Bincode's derived representation is positional,
/// so current Postcard DTO changes must not alter the V3 migration reader.
#[derive(Decode)]
#[cfg_attr(test, derive(Encode))]
pub(super) struct StoredMessageV3 {
    pub(super) id: String,
    pub(super) sender: String,
    pub(super) display_name: String,
    pub(super) content: String,
    pub(super) emote_lookup_channel: Option<String>,
    pub(super) emote_lookup_source_id: Option<String>,
    pub(super) color: Option<String>,
    pub(super) badges: Vec<String>,
    pub(super) timestamp: i64,
    pub(super) user_id: Option<String>,
    pub(super) twitch_emotes_tag: Option<String>,
    pub(super) reply_emote_offset: usize,
    pub(super) is_action: bool,
    pub(super) reply_parent_msg_id: Option<String>,
    pub(super) reply_parent_user_login: Option<String>,
    pub(super) reply_parent_display_name: Option<String>,
    pub(super) reply_parent_msg_body: Option<String>,
    pub(super) reply_parent_user_id: Option<String>,
}

/// Exact DTO written with `HISTORY_CACHE_V2_MAGIC`.
#[derive(Decode)]
#[cfg_attr(test, derive(Encode))]
pub(super) struct StoredMessageV2 {
    pub(super) id: String,
    pub(super) sender: String,
    pub(super) display_name: String,
    pub(super) content: String,
    pub(super) color: Option<String>,
    pub(super) badges: Vec<String>,
    pub(super) timestamp: i64,
    pub(super) user_id: Option<String>,
    pub(super) twitch_emotes_tag: Option<String>,
    pub(super) reply_emote_offset: usize,
    pub(super) is_action: bool,
    pub(super) reply_parent_msg_id: Option<String>,
    pub(super) reply_parent_user_login: Option<String>,
    pub(super) reply_parent_display_name: Option<String>,
    pub(super) reply_parent_msg_body: Option<String>,
    pub(super) reply_parent_user_id: Option<String>,
}

/// Exact DTO written before `HISTORY_CACHE_V2_MAGIC` was introduced.
///
/// Keep this field order stable: bincode's derived representation is positional,
/// so adding a field to `StoredMessage` without an explicit fallback would make
/// every existing local history file unreadable.
#[derive(Decode)]
#[cfg_attr(test, derive(Encode))]
pub(super) struct StoredMessageV1 {
    pub(super) id: String,
    pub(super) sender: String,
    pub(super) display_name: String,
    pub(super) content: String,
    pub(super) color: Option<String>,
    pub(super) badges: Vec<String>,
    pub(super) timestamp: i64,
    pub(super) user_id: Option<String>,
    pub(super) twitch_emotes_tag: Option<String>,
    pub(super) reply_emote_offset: usize,
    pub(super) is_action: bool,
    pub(super) reply_parent_msg_id: Option<String>,
    pub(super) reply_parent_user_login: Option<String>,
    pub(super) reply_parent_display_name: Option<String>,
    pub(super) reply_parent_msg_body: Option<String>,
}

impl From<StoredMessageV1> for StoredMessage {
    fn from(value: StoredMessageV1) -> Self {
        Self {
            twitch_gifs_tag: None,
            id: value.id,
            sender: value.sender,
            display_name: value.display_name,
            content: value.content,
            emote_lookup_channel: None,
            emote_lookup_source_id: None,
            color: value.color,
            badges: value.badges,
            timestamp: value.timestamp,
            user_id: value.user_id,
            twitch_emotes_tag: value.twitch_emotes_tag,
            reply_emote_offset: value.reply_emote_offset,
            is_action: value.is_action,
            reply_parent_msg_id: value.reply_parent_msg_id,
            reply_parent_user_login: value.reply_parent_user_login,
            reply_parent_display_name: value.reply_parent_display_name,
            reply_parent_msg_body: value.reply_parent_msg_body,
            reply_parent_user_id: None,
        }
    }
}

impl From<StoredMessageV2> for StoredMessage {
    fn from(value: StoredMessageV2) -> Self {
        Self {
            twitch_gifs_tag: None,
            id: value.id,
            sender: value.sender,
            display_name: value.display_name,
            content: value.content,
            emote_lookup_channel: None,
            emote_lookup_source_id: None,
            color: value.color,
            badges: value.badges,
            timestamp: value.timestamp,
            user_id: value.user_id,
            twitch_emotes_tag: value.twitch_emotes_tag,
            reply_emote_offset: value.reply_emote_offset,
            is_action: value.is_action,
            reply_parent_msg_id: value.reply_parent_msg_id,
            reply_parent_user_login: value.reply_parent_user_login,
            reply_parent_display_name: value.reply_parent_display_name,
            reply_parent_msg_body: value.reply_parent_msg_body,
            reply_parent_user_id: value.reply_parent_user_id,
        }
    }
}

impl From<StoredMessageV3> for StoredMessage {
    fn from(value: StoredMessageV3) -> Self {
        Self {
            twitch_gifs_tag: None,
            id: value.id,
            sender: value.sender,
            display_name: value.display_name,
            content: value.content,
            emote_lookup_channel: value.emote_lookup_channel,
            emote_lookup_source_id: value.emote_lookup_source_id,
            color: value.color,
            badges: value.badges,
            timestamp: value.timestamp,
            user_id: value.user_id,
            twitch_emotes_tag: value.twitch_emotes_tag,
            reply_emote_offset: value.reply_emote_offset,
            is_action: value.is_action,
            reply_parent_msg_id: value.reply_parent_msg_id,
            reply_parent_user_login: value.reply_parent_user_login,
            reply_parent_display_name: value.reply_parent_display_name,
            reply_parent_msg_body: value.reply_parent_msg_body,
            reply_parent_user_id: value.reply_parent_user_id,
        }
    }
}
