//! Moderation Log component (Logic Only)
use crate::backend::lock_utils::RwLockExt;
use chrono::Utc;
use std::sync::LazyLock;
use std::sync::RwLock;

/// Type of moderation action
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModActionType {
    Ban,
    Unban,
    Timeout,
    Delete,
    Clear,
    Mod,
    Unmod,
    Vip,
    Unvip,
    Warning,
    ShieldEnable,
    ShieldDisable,
}

/// A moderation action entry
#[derive(Debug, Clone)]
pub struct ModAction {
    pub action_type: ModActionType,
    pub username: String,
    pub channel: String,
    pub details: String,
    pub timestamp: i64,
}

/// Parse a system message into a mod action (if applicable)
pub fn parse_mod_action(
    _id: &str,
    text: &str,
    author_name: &str,
    channel: &str,
    timestamp: i64,
) -> Option<ModAction> {
    if text.contains("permanently banned") {
        return Some(ModAction {
            action_type: ModActionType::Ban,
            username: author_name.to_string(),
            channel: channel.to_string(),
            details: text.to_string(),
            timestamp,
        });
    }

    if text.contains("was unbanned") {
        return Some(ModAction {
            action_type: ModActionType::Unban,
            username: author_name.to_string(),
            channel: channel.to_string(),
            details: text.to_string(),
            timestamp,
        });
    }

    if text.contains("timed out") {
        return Some(ModAction {
            action_type: ModActionType::Timeout,
            username: author_name.to_string(),
            channel: channel.to_string(),
            details: text.to_string(),
            timestamp,
        });
    }

    if text.contains("Message deleted") || text.contains("was deleted") {
        return Some(ModAction {
            action_type: ModActionType::Delete,
            username: author_name.to_string(),
            channel: channel.to_string(),
            details: text.to_string(),
            timestamp,
        });
    }

    if text.contains("Chat was cleared") {
        return Some(ModAction {
            action_type: ModActionType::Clear,
            username: "moderator".to_string(),
            channel: channel.to_string(),
            details: text.to_string(),
            timestamp,
        });
    }

    None
}

/// Global mod action store
pub static MOD_ACTIONS: LazyLock<RwLock<Vec<ModAction>>> =
    LazyLock::new(|| RwLock::new(Vec::new()));

const MOD_ACTION_DUPLICATE_WINDOW_MS: u64 = 3_000;

/// Add a mod action to the store
pub fn add_mod_action(action: ModAction) {
    let mut actions = MOD_ACTIONS.write_or_recover();
    if is_recent_duplicate(&actions, &action) {
        return;
    }

    let action = sanitize_mod_action(action);
    actions.insert(0, action);
    if actions.len() > 100 {
        actions.truncate(100);
    }
}

/// Record a locally initiated moderation action so overlay-driven moderation
/// updates appear immediately instead of waiting for a matching system row.
pub fn record_local_mod_action(
    action_type: ModActionType,
    username: impl Into<String>,
    channel: impl Into<String>,
    details: impl Into<String>,
) {
    add_mod_action(ModAction {
        action_type,
        username: username.into(),
        channel: channel.into(),
        details: details.into(),
        timestamp: Utc::now().timestamp_millis(),
    });
}

/// Best-effort current ban state for a user, derived from the most recent
/// recorded Ban/Unban/Timeout action for them (the store is newest-first and
/// fed by both CLEARCHAT system messages and locally issued commands).
/// Timeouts count as banned while their parsed duration is still running.
/// The store holds only the last 100 actions, so this is a heuristic, not
/// ground truth — unknown users default to "not banned".
pub fn is_user_likely_banned(channel: &str, username: &str) -> bool {
    let actions = MOD_ACTIONS.read_or_recover();
    let now_ms = Utc::now().timestamp_millis();
    for action in actions.iter() {
        if !action.username.eq_ignore_ascii_case(username)
            || !action.channel.eq_ignore_ascii_case(channel)
        {
            continue;
        }
        match action.action_type {
            ModActionType::Ban => return true,
            ModActionType::Unban => return false,
            ModActionType::Timeout => {
                let seconds = action
                    .details
                    .split_whitespace()
                    .find_map(|token| token.parse::<i64>().ok());
                return match seconds {
                    Some(seconds) => {
                        now_ms
                            < action
                                .timestamp
                                .saturating_add(seconds.saturating_mul(1000))
                    }
                    None => false,
                };
            }
            _ => continue,
        }
    }
    false
}

fn is_recent_duplicate(actions: &[ModAction], action: &ModAction) -> bool {
    if matches!(action.action_type, ModActionType::Delete) {
        return false;
    }

    actions.iter().any(|existing| {
        existing.action_type == action.action_type
            && existing.username.eq_ignore_ascii_case(&action.username)
            && existing.channel.eq_ignore_ascii_case(&action.channel)
            && existing.timestamp.abs_diff(action.timestamp) <= MOD_ACTION_DUPLICATE_WINDOW_MS
    })
}

fn sanitize_mod_action(mut action: ModAction) -> ModAction {
    if !crate::backend::streamer_mode::should_hide_mod_actions() {
        return action;
    }

    match action.action_type {
        ModActionType::Ban => {
            action.username = "Hidden user".to_string();
            action.channel = "hidden".to_string();
            action.details = "Ban details hidden in streamer mode".to_string();
        }
        ModActionType::Unban => {
            action.username = "Hidden user".to_string();
            action.channel = "hidden".to_string();
            action.details = "Unban details hidden in streamer mode".to_string();
        }
        ModActionType::Timeout => {
            action.username = "Hidden user".to_string();
            action.channel = "hidden".to_string();
            action.details = "Timeout details hidden in streamer mode".to_string();
        }
        ModActionType::Delete => {
            action.username = "Hidden user".to_string();
            action.channel = "hidden".to_string();
            action.details = "Deleted-message details hidden in streamer mode".to_string();
        }
        ModActionType::Clear => {
            action.username = "Hidden channel".to_string();
            action.channel = "hidden".to_string();
            action.details = "Chat-clear details hidden in streamer mode".to_string();
        }
        ModActionType::Mod => {
            action.username = "Hidden user".to_string();
            action.channel = "hidden".to_string();
            action.details = "Moderator details hidden in streamer mode".to_string();
        }
        ModActionType::Unmod => {
            action.username = "Hidden user".to_string();
            action.channel = "hidden".to_string();
            action.details = "Moderator details hidden in streamer mode".to_string();
        }
        ModActionType::Vip => {
            action.username = "Hidden user".to_string();
            action.channel = "hidden".to_string();
            action.details = "VIP details hidden in streamer mode".to_string();
        }
        ModActionType::Unvip => {
            action.username = "Hidden user".to_string();
            action.channel = "hidden".to_string();
            action.details = "VIP details hidden in streamer mode".to_string();
        }
        ModActionType::Warning => {
            action.username = "Hidden user".to_string();
            action.channel = "hidden".to_string();
            action.details = "Warning details hidden in streamer mode".to_string();
        }
        ModActionType::ShieldEnable => {
            action.username = "Hidden channel".to_string();
            action.channel = "hidden".to_string();
            action.details = "Shield Mode details hidden in streamer mode".to_string();
        }
        ModActionType::ShieldDisable => {
            action.username = "Hidden channel".to_string();
            action.channel = "hidden".to_string();
            action.details = "Shield Mode details hidden in streamer mode".to_string();
        }
    }

    action
}

/// Get all mod actions
pub fn get_mod_actions() -> Vec<ModAction> {
    MOD_ACTIONS.read_or_recover().clone()
}

/// Clear all mod actions
pub fn clear_mod_actions() {
    MOD_ACTIONS.write_or_recover().clear();
}
