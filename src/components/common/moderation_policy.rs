//! Fail-closed UI admission for irreversible moderation actions.
//!
//! Presentation surfaces may have different target metadata, but they all use
//! this one authority check before showing an action and immediately before
//! invoking the bridge. Normal chat, copy, profile, and whisper affordances do
//! not go through this policy.

use crate::backend;

/// Whether the active authenticated viewer may moderate this target in this
/// channel. Unknown identity/role information denies the action.
pub fn can_moderate_target(channel: &str, target: &str, target_badges: &[String]) -> bool {
    can_moderate_target_with_broadcaster_target(
        channel,
        target,
        target_badges
            .iter()
            .any(|badge| backend::badges::badge_key_matches(badge, "broadcaster")),
    )
}

/// Equivalent to [`can_moderate_target`] when the caller has already derived
/// whether the target carries a broadcaster badge. This avoids building an
/// allocation solely to combine badge sources during rendering.
pub fn can_moderate_target_with_broadcaster_target(
    channel: &str,
    target: &str,
    is_broadcaster_target: bool,
) -> bool {
    let channel = channel.trim().trim_start_matches('#');
    let target = target.trim().trim_start_matches('@');
    let viewer = backend::settings::get_username_lowercase();

    !channel.is_empty()
        && !target.is_empty()
        && !viewer.is_empty()
        && backend::irc::can_moderate_channel(channel)
        && !viewer.eq_ignore_ascii_case(target)
        && !channel.eq_ignore_ascii_case(target)
        && !is_broadcaster_target
}

/// Recheck at the effect boundary. Callers should also use
/// [`can_moderate_target`] for render-time admission so unauthorized controls
/// never enter the UI in the first place.
pub fn recheck_moderation_target(channel: &str, target: &str) -> bool {
    can_moderate_target(channel, target, &[])
}

/// Whether the active authenticated viewer may change this target's channel
/// role. Twitch's moderator and VIP mutation endpoints require the token owner
/// to be the broadcaster, so ordinary channel moderators must never receive
/// these controls even though they may ban or time out users.
pub fn can_manage_role_target(channel: &str, target: &str) -> bool {
    let channel = channel.trim().trim_start_matches('#');
    let target = target.trim().trim_start_matches('@');
    let viewer = backend::settings::get_username_lowercase();

    !channel.is_empty()
        && !target.is_empty()
        && !viewer.is_empty()
        && viewer.eq_ignore_ascii_case(channel)
        && !viewer.eq_ignore_ascii_case(target)
}

/// Recheck broadcaster-only role authority immediately before invoking a
/// moderator/VIP bridge mutation.
pub fn recheck_role_management_target(channel: &str, target: &str) -> bool {
    can_manage_role_target(channel, target)
}
