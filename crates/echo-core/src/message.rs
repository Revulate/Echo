use crate::slot::MessageSlot;
use serde::{Deserialize, Serialize};
use smallvec::SmallVec;
use std::sync::Arc;
use uuid::Uuid;

/// Case-insensitive substring search for an ASCII needle without allocating.
///
/// Used for per-message mention checks: Twitch logins are ASCII, so lowering
/// the entire message content with `to_lowercase()` (a full String copy per
/// message) is unnecessary. The needle must already be lowercase.
pub fn contains_ascii_case_insensitive(haystack: &str, needle_lower: &str) -> bool {
    let needle = needle_lower.as_bytes();
    if needle.is_empty() {
        return false;
    }
    haystack
        .as_bytes()
        .windows(needle.len())
        .any(|window| window.eq_ignore_ascii_case(needle))
}

/// Type of highlight applied to a message
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum HighlightType {
    Whisper,
    Mention,
    User {
        username: String,
    },
    ParticipatedReply {
        username: String,
    },
    Badge {
        badge: String,
    },
    HighlightedMessage,
    FirstMessage,
    ReturningChatter,
    Subscription,
    Raid,
    BitsBadge,
    Announcement {
        /// Twitch Helix announcement color variant (PRIMARY/BLUE/GREEN/ORANGE/PURPLE).
        #[serde(default)]
        color: Option<String>,
    },
    WatchStreak,
    Phrase {
        pattern: String,
    },
    CustomPhrase {
        pattern: String,
        color: Option<String>,
    },
    CustomUser {
        username: String,
        color: Option<String>,
    },
    CustomBadge {
        badge: String,
        color: Option<String>,
    },
    SelfMessage,
    /// Paid/elevated chat or Hype Chat message.
    Elevated,
    ChannelPointRedeem {
        reward_id: String,
        reward_title: String,
        reward_cost: u32,
    },
}

/// Reply information for messages that are replies to other messages
///
/// PERF: Uses Arc<str> for zero-cost cloning, matching ChatMessage's string strategy
#[derive(Clone, Debug, PartialEq, Default)]
pub struct ReplyInfo {
    pub parent_msg_id: Arc<str>,
    pub parent_user_login: Arc<str>,
    pub parent_display_name: Arc<str>,
    /// Optional Twitch ID supplied by reply metadata; it is only consumed by
    /// cosmetic display-name rules and never replaces the canonical login.
    pub parent_user_id: Option<Arc<str>>,
    pub parent_msg_body: Option<Arc<str>>,
}

/// Prefix of the synthetic id carried by derived day-divider display rows.
///
/// Day dividers are built by `ChatState`'s display projection and never enter
/// the authoritative channel buffer, so this prefix cannot collide with a Twitch
/// message id (UUID) or an optimistic echo id (`optimistic-…`).
pub const DAY_DIVIDER_ID_PREFIX: &str = "__echo_day__";

/// Build the stable synthetic id of the day divider that precedes
/// `next_message_id`: `__echo_day__:{channel}:{YYYY-MM-DD}:{next_real_message_id}`.
///
/// Keying the id on the following real message keeps it stable across history
/// pages, so a boundary that is already on screen is not duplicated when older
/// messages are prepended.
pub fn day_divider_id(channel: &str, date_iso: &str, next_message_id: &str) -> String {
    format!("{DAY_DIVIDER_ID_PREFIX}:{channel}:{date_iso}:{next_message_id}")
}

/// Type of system message
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SystemMessageType {
    /// Upload started
    UploadStart,
    /// Upload completed successfully
    UploadSuccess {
        url: String,
        delete_url: Option<String>,
    },
    /// Upload failed
    UploadError { error: String },
    /// Informational message
    Info,
    /// Error message
    Error { message: String },
    /// Connection status change
    ConnectionStatus { status: String },
}

/// How moderator-removed (CLEARMSG, timeout, ban, chat clear) rows are shown.
///
/// The backend always keeps the original row and marks it deleted; this mode
/// is a presentation choice made by the global setting or overridden per split.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum DeletedMessageMode {
    /// Show removed rows as if nothing happened.
    Disable,
    /// Remove them from the split.
    Hide,
    /// Collapse each to `<message deleted>`; a click reveals the original.
    Replace,
    /// Fade and strike through the original text.
    Strikethrough,
    /// Fade the original text.
    #[default]
    Dim,
}

/// How a deleted row looks once a split has applied its [`DeletedMessageMode`].
/// Splits derive the row they paint, so the shared layout cache never has to
/// know which split asked.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum DeletedPresentation {
    /// Original text, faded.
    #[default]
    Dimmed,
    /// Original text, faded and struck through.
    Struck,
    /// `<message deleted>` stand-in that a click expands.
    Collapsed,
}

/// Flags for message rendering behavior
#[derive(Clone, Debug, PartialEq, Default)]
pub struct MessageFlags {
    pub is_action: bool,                       // /me action message
    pub highlight_type: Option<HighlightType>, // Highlight type for colored bars
    pub is_system: bool,                       // System message (sub notification, etc.)
    pub is_usernotice: bool, // Twitch USERNOTICE row (stable ID, but not a reply/delete target)
    pub is_first_message: bool,
    pub system_type: Option<SystemMessageType>, // Type of system message with associated data
    pub is_shared_chat: bool,                   // Message from shared chat (different source room)
    pub is_deleted: bool, // Message was removed by Twitch CLEARMSG, a timeout, a ban or a chat clear
    /// Presentation of a deleted row; only read while `is_deleted` is set.
    pub deleted_presentation: DeletedPresentation,
    pub is_elevated_message: bool, // Twitch paid/elevated chat message
    pub is_hype_chat: bool,        // Twitch Hype Chat / paid pinned chat message
    pub is_automod: bool,          // AutoMod held/updated system row
    pub is_automod_pending: bool, // AutoMod hold awaiting a moderator decision (renders inline Allow/Deny)
    pub is_suspicious_user: bool, // Suspicious-user system row
    pub is_restricted: bool,      // Restricted suspicious-user state
    pub is_monitored: bool,       // Monitored suspicious-user state
    /// Similarity detector flagged this message as repeating recent chat.
    /// Such rows are dimmed (shaded toward the background) instead of
    /// highlighted; set only when no stronger highlight applies and the
    /// "Shade similar messages" option is enabled.
    pub is_similar: bool,
    /// Zebra-striping parity, assigned when the message enters its channel
    /// buffer. Stored on the message rather than derived from the row index
    /// so pruning the buffer front does not flip the parity of every visible
    /// row.
    pub is_alternate_row: bool,
}

/// Complete chat message with all parsed data
///
/// PERF: Uses Arc<str> for string fields to reduce memory allocations
/// and enable efficient sharing across message clones. SmallVec for badges
/// avoids heap allocation for the common case of 1-4 badges.
#[derive(Clone, Debug)]
pub struct ChatMessage {
    pub id: Arc<str>,
    pub channel: Arc<str>,
    /// Channel catalog used to resolve third-party emotes for this row.
    /// `None` means the owning `channel`; `Some("")` intentionally means
    /// global/user catalogs only for an unresolved shared-chat source.
    pub emote_lookup_channel: Option<Arc<str>>,
    /// Source room id retained only while a shared-chat source login is unresolved.
    pub emote_lookup_source_id: Option<Arc<str>>,
    pub sender: Arc<str>,
    pub display_name: Arc<str>,
    pub content: Arc<str>,
    pub color: Option<Arc<str>>,
    pub badges: SmallVec<[Arc<str>; 4]>, // "name:version"
    pub timestamp: i64,
    /// Message slots - immutable structure, emotes resolved at render time
    pub slots: Arc<[MessageSlot]>,
    /// Pre-computed structure hash for cache invalidation
    pub structure_hash: u64,
    pub reply: Option<ReplyInfo>,
    pub flags: MessageFlags,
    /// IRC emotes tag stored for Twitch emote position lookups
    pub twitch_emotes_tag: Option<Arc<str>>,
    /// Original Twitch IRC GIF annotations. URLs and scalar ranges are retained
    /// verbatim so history and reparsing preserve the provider's media identity.
    pub twitch_gifs_tag: Option<Arc<str>>,
    /// Twitch user ID (from user-id IRC tag)
    pub user_id: Option<Arc<str>>,
    /// Offset applied to twitch_emotes_tag positions due to reply mention stripping.
    /// When retokenizing, subtract this from parsed positions to get correct offsets.
    /// This fixes a bug where reply messages with Twitch emotes would break after
    /// retokenization because the raw emote tag has unadjusted positions.
    pub reply_emote_offset: usize,
}

impl ChatMessage {
    /// Whether Twitch accepts reply/delete/moderation actions against this row.
    /// USERNOTICE IDs are stable for display deduplication but are not ordinary
    /// chat-message targets.
    pub fn allows_remote_message_actions(&self) -> bool {
        !self.flags.is_system
            && !self.flags.is_usernotice
            && !matches!(
                self.flags.highlight_type.as_ref(),
                Some(HighlightType::Whisper)
            )
    }

    /// ISO `YYYY-MM-DD` label when this row is a derived day divider.
    ///
    /// Day dividers are display-only rows produced by `ChatState`'s projection:
    /// they never enter the authoritative buffer, carry no content, sender,
    /// badges, or slots, and are excluded from retention, unread counts, search,
    /// replies, moderation, and exports simply by never existing there. The date
    /// travels in the synthetic id (see [`day_divider_id`]) so no extra
    /// per-message allocation or model field is required.
    pub fn day_divider_date(&self) -> Option<&str> {
        if !self.flags.is_system {
            return None;
        }
        // `{channel}:{YYYY-MM-DD}:{next_real_message_id}` — Twitch logins and ISO
        // dates never contain ':', so the first two fields split unambiguously.
        let rest = self
            .id
            .strip_prefix(DAY_DIVIDER_ID_PREFIX)?
            .strip_prefix(':')?;
        let mut parts = rest.splitn(3, ':');
        let _channel = parts.next()?;
        let date_iso = parts.next()?;
        (!date_iso.is_empty()).then_some(date_iso)
    }
}

impl Default for ChatMessage {
    fn default() -> Self {
        Self {
            id: Uuid::new_v4().to_string().into(),
            channel: Arc::from(""),
            emote_lookup_channel: None,
            emote_lookup_source_id: None,
            sender: Arc::from(""),
            display_name: Arc::from(""),
            content: Arc::from(""),
            color: None,
            badges: SmallVec::new(),
            timestamp: 0,
            slots: Arc::from([]),
            structure_hash: 0,
            reply: None,
            flags: MessageFlags::default(),
            twitch_emotes_tag: None,
            twitch_gifs_tag: None,
            user_id: None,
            reply_emote_offset: 0,
        }
    }
}
