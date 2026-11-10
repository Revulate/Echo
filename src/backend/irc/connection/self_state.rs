//! The signed-in user's per-channel state (USERSTATE color and badges).

use super::*;

/// The signed-in user's name color and badge keys in one channel, from its
/// USERSTATE.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SelfChannelState {
    pub color: Option<String>,
    pub badges: Vec<String>,
}

impl SelfChannelState {
    /// Holds a moderator (or lead moderator) or broadcaster badge here.
    pub fn moderates(&self) -> bool {
        self.badges.iter().any(|badge| {
            crate::backend::badges::is_moderator_badge_key(badge)
                || crate::backend::badges::badge_key_matches(badge, "broadcaster")
        })
    }
}

/// [`SelfChannelState`] per lowercase channel login.
pub(super) static SELF_CHANNEL_STATE: LazyLock<Mutex<FxHashMap<String, SelfChannelState>>> =
    LazyLock::new(|| Mutex::new(FxHashMap::default()));

/// Cache self user's color and badges for a channel (from USERSTATE)
pub fn cache_self_state(channel: &str, color: Option<String>, badges: Vec<String>) -> bool {
    let visibility_commit = crate::backend::blocked_users::visibility_commit_guard();
    let previous_blocked_visibility_role = blocked_visibility_role_state(channel);
    let next = SelfChannelState { color, badges };
    let previous = SELF_CHANNEL_STATE
        .lock()
        .insert(channel.to_lowercase(), next.clone());
    let changed = previous.as_ref() != Some(&next);
    let next_blocked_visibility_role = blocked_visibility_role_state(channel);
    let blocked_rows_changed = previous_blocked_visibility_role != next_blocked_visibility_role
        && apply_blocked_user_filter_to_channel(channel);
    drop(visibility_commit);
    if blocked_rows_changed && !crate::backend::blocked_users::initial_visibility_pending() {
        dispatch_blocked_channel_refreshes(HashSet::from([channel.to_lowercase()]));
    }
    changed
}

/// Clear role state at an IRC-session boundary under the same gate used by
/// message producers. This prevents a producer from observing a half-published
/// moderator/broadcaster transition while committing irreversible effects.
pub(super) fn clear_self_channel_states() {
    let visibility_commit = crate::backend::blocked_users::visibility_commit_guard();
    SELF_CHANNEL_STATE.lock().clear();
    drop(visibility_commit);
    crate::backend::irc::room_state::reset_send_exemptions();
}

/// Get self user's cached state for a channel (color, badges)
pub fn get_self_state(channel: &str) -> Option<SelfChannelState> {
    SELF_CHANNEL_STATE
        .lock()
        .get(&channel.to_lowercase())
        .cloned()
}

/// Whether the active viewer may moderate this channel. Broadcaster ownership
/// is known from the authenticated login; moderator authority fails closed
/// until Twitch supplies the channel's USERSTATE badges.
pub fn can_moderate_channel(channel: &str) -> bool {
    let channel = channel.trim().trim_start_matches('#');
    if channel.is_empty() {
        return false;
    }
    if crate::backend::settings::get_username_lowercase().eq_ignore_ascii_case(channel) {
        return true;
    }
    get_self_state(channel).is_some_and(|state| state.moderates())
}

/// Process a USERSTATE message
pub(super) fn process_userstate_message(
    msg: twitch_irc::message::UserStateMessage,
    session_login: &str,
) {
    let color = msg
        .name_color
        .map(|c| format!("#{:02x}{:02x}{:02x}", c.r, c.g, c.b));
    let badges: Vec<String> = msg
        .badges
        .iter()
        .map(|b| format!("{}:{}", b.name, b.version))
        .collect();
    crate::log_irc!(
        debug,
        "USERSTATE for #{}: color={:?}, badges={:?}",
        msg.channel_login,
        color,
        badges
    );
    let slow_mode_exempt = msg
        .source
        .tags
        .0
        .get("mod")
        .is_some_and(|mod_flag| mod_flag == "1")
        || session_login.eq_ignore_ascii_case(&msg.channel_login)
        || badges.iter().any(|badge| {
            crate::backend::badges::is_moderator_badge_key(badge)
                || crate::backend::badges::badge_key_matches(badge, "broadcaster")
                || crate::backend::badges::badge_key_matches(badge, "vip")
        });
    crate::backend::irc::room_state::note_self_roles(
        session_login,
        &msg.channel_login,
        slow_mode_exempt,
    );
    if cache_self_state(&msg.channel_login, color, badges) {
        crate::backend::eventsub::refresh_channel_authorization(&msg.channel_login);
    }
}
