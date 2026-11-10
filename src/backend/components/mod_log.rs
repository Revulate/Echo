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
    /// Length of a timeout, when the source reported one.
    pub timeout_secs: Option<u64>,
    /// Streamer mode replaced the user, channel and details with placeholders.
    pub masked: bool,
}

impl ModAction {
    pub fn new(
        action_type: ModActionType,
        username: impl Into<String>,
        channel: impl Into<String>,
        details: impl Into<String>,
        timestamp: i64,
    ) -> Self {
        Self {
            action_type,
            username: username.into(),
            channel: channel.into(),
            details: details.into(),
            timestamp,
            timeout_secs: None,
            masked: false,
        }
    }

    pub fn with_timeout_secs(mut self, timeout_secs: Option<u64>) -> Self {
        self.timeout_secs = timeout_secs;
        self
    }
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
    add_mod_action(ModAction::new(
        action_type,
        username,
        channel,
        details,
        Utc::now().timestamp_millis(),
    ));
}

/// [`record_local_mod_action`] for a timeout of known length.
pub fn record_local_timeout(
    username: impl Into<String>,
    channel: impl Into<String>,
    details: impl Into<String>,
    timeout_secs: u64,
) {
    add_mod_action(
        ModAction::new(
            ModActionType::Timeout,
            username,
            channel,
            details,
            Utc::now().timestamp_millis(),
        )
        .with_timeout_secs(Some(timeout_secs)),
    );
}

/// Best-effort current ban state for a user, derived from the most recent
/// recorded Ban/Unban/Timeout action for them (the store is newest-first and
/// fed by both CLEARCHAT system messages and locally issued commands).
/// Timeouts count as banned while their recorded length is still running.
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
                return action.timeout_secs.is_some_and(|seconds| {
                    let seconds = i64::try_from(seconds).unwrap_or(i64::MAX);
                    now_ms
                        < action
                            .timestamp
                            .saturating_add(seconds.saturating_mul(1000))
                });
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
    action.masked = true;

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
