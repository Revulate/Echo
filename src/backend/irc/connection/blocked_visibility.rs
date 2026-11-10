//! Hiding and revealing rows from blocked users in channel buffers.

use super::*;

/// Apply the channel-role exception only after Twitch has supplied USERSTATE.
/// A known blocked sender fails closed while moderator state is unknown: an
/// early row can otherwise escape into logs, alerts, and media requests before
/// USERSTATE arrives. Broadcaster ownership is known from the active login.
pub(crate) struct BlockedChannelVisibilitySnapshot {
    pub(super) policy: Option<crate::backend::blocked_users::BlockedVisibilityPolicy>,
    pub(super) hide_blocked_senders: bool,
}

impl BlockedChannelVisibilitySnapshot {
    #[inline]
    pub(crate) fn should_hide_sender(&self, user_id: Option<&str>, user_login: &str) -> bool {
        self.hide_blocked_senders
            && self
                .policy
                .as_ref()
                .is_some_and(|policy| policy.contains(user_id, user_login))
    }

    #[inline]
    pub(crate) fn should_redact_reply_parent(&self, parent_login: &str) -> bool {
        self.policy
            .as_ref()
            .is_some_and(|policy| policy.contains(None, parent_login))
    }
}

pub(crate) fn blocked_channel_visibility_snapshot(
    channel: &str,
) -> BlockedChannelVisibilitySnapshot {
    let policy = crate::backend::blocked_users::visibility_policy_snapshot();
    let hide_blocked_senders = policy.as_ref().is_some_and(|policy| {
        !blocked_visibility_role_state_for(channel, policy.visibility).unwrap_or(false)
    });
    BlockedChannelVisibilitySnapshot {
        policy,
        hide_blocked_senders,
    }
}

pub(crate) fn should_hide_blocked_channel_message(
    user_id: Option<&str>,
    user_login: &str,
    channel: &str,
) -> bool {
    blocked_channel_visibility_snapshot(channel).should_hide_sender(user_id, user_login)
}

/// `None` means the role-dependent policy is waiting for the first USERSTATE.
/// Broadcasters are known from the authenticated login before badges arrive.
pub(super) fn blocked_visibility_role_state_for(
    channel: &str,
    visibility: crate::backend::settings::BlockedUserVisibility,
) -> Option<bool> {
    use crate::backend::settings::BlockedUserVisibility;
    let channel = channel.trim().trim_start_matches('#');
    let is_channel_owner = !channel.is_empty()
        && crate::backend::settings::get_username_lowercase().eq_ignore_ascii_case(channel);
    match visibility {
        BlockedUserVisibility::Never => Some(false),
        BlockedUserVisibility::IfModerator => {
            if is_channel_owner {
                return Some(true);
            }
            get_self_state(channel).map(|state| state.moderates())
        }
        BlockedUserVisibility::IfBroadcaster => Some(is_channel_owner),
    }
}

pub(super) fn blocked_visibility_role_state(channel: &str) -> Option<bool> {
    blocked_visibility_role_state_for(
        channel,
        crate::backend::settings::get_show_blocked_users_messages(),
    )
}

/// The membership publisher already owns the visibility commit gate. Keeping
/// this non-locking entry point avoids self-deadlock while preserving the
/// global gate -> membership -> chat/whisper lock order.
pub(crate) fn apply_blocked_user_filter_to_buffers_visibility_commit_held(
    _commit: &crate::backend::blocked_users::VisibilityCommit,
) -> HashSet<String> {
    let Some(policy) = crate::backend::blocked_users::visibility_policy_snapshot() else {
        return HashSet::new();
    };
    if policy.is_empty() {
        return HashSet::new();
    }

    let channels: Vec<String> = CHAT_MESSAGES
        .iter()
        .map(|entry| entry.key().clone())
        .collect();
    let expected_changed_channels: HashSet<String> = channels
        .iter()
        .filter(|channel| blocked_user_filter_would_change_channel(channel, &policy))
        .cloned()
        .collect();
    mark_blocked_channel_refresh_handoff(&expected_changed_channels);
    let mut changed_channels = HashSet::new();
    for channel in channels {
        if apply_blocked_user_filter_to_channel_with_policy(&channel, &policy) {
            changed_channels.insert(channel);
        }
    }

    crate::backend::whispers::remove_blocked_users_with_policy(&policy);
    let late_changes: HashSet<String> = changed_channels
        .difference(&expected_changed_channels)
        .cloned()
        .collect();
    mark_blocked_channel_refresh_handoff(&late_changes);
    changed_channels.extend(expected_changed_channels);
    changed_channels
}

pub(super) fn mark_blocked_channel_refresh_handoff(channels: &HashSet<String>) {
    if channels.is_empty() {
        return;
    }
    let observers = crate::backend::observers::get_observer_registry();
    crate::backend::blocked_users::mark_chat_state_reconcile_pending(
        channels
            .iter()
            .filter(|channel| observers.has_live_observers(channel)),
    );
}

pub(crate) fn dispatch_blocked_channel_refreshes(channels: HashSet<String>) {
    if channels.is_empty() {
        return;
    }
    let observers = crate::backend::observers::get_observer_registry();
    bump_chat_version();
    for channel in channels {
        observers.notify(
            &channel,
            crate::backend::observers::ChannelEvent::MessagesRetokenized,
        );
    }
}

/// A new account has no authoritative block list yet. Rendering and direct
/// bridge reads consult the revoke flag immediately; observer wakes ensure all
/// retained ChatState snapshots reconcile on the next UI turn.
pub(crate) fn notify_blocked_resident_visibility_revoked() {
    let channels: HashSet<String> = CHAT_MESSAGES
        .iter()
        .map(|entry| entry.key().clone())
        .collect();
    mark_blocked_channel_refresh_handoff(&channels);
    dispatch_blocked_channel_refreshes(channels);
}

pub(super) fn remove_pending_blocked_visibility_rows(
    rows: Vec<crate::backend::blocked_users::PendingChannelRow>,
) {
    let mut rows_by_channel: HashMap<String, HashSet<String>> = HashMap::new();
    for row in rows {
        rows_by_channel
            .entry(row.channel)
            .or_default()
            .insert(row.message_id);
    }
    for (channel, message_ids) in rows_by_channel {
        if let Some(mut buffer) = CHAT_MESSAGES.get_mut(&channel) {
            buffer.retain(|message| !message_ids.contains(message.id.as_ref()));
        }
    }
}

/// Account transitions discard backend-only rows that were never made
/// authoritative. The caller owns the visibility commit gate.
pub(crate) fn discard_pending_blocked_visibility_rows_commit_held(
    _commit: &crate::backend::blocked_users::VisibilityCommit,
    rows: Vec<crate::backend::blocked_users::PendingChannelRow>,
) {
    remove_pending_blocked_visibility_rows(rows);
}

pub(super) fn pending_channel_row_capacity_available_commit_held(
    commit: &crate::backend::blocked_users::VisibilityCommit,
) -> bool {
    if crate::backend::blocked_users::pending_channel_capacity_remaining_commit_held(commit) == 0 {
        log_pending_channel_overflow();
        false
    } else {
        true
    }
}

pub(super) fn log_pending_channel_overflow() {
    if crate::backend::blocked_users::should_log_pending_channel_overflow() {
        crate::log_irc!(
            warn,
            "Blocked users: Pending channel-row queue reached its bounded capacity; dropping new backend-only rows until authority resolves"
        );
    }
}

pub(super) fn defer_buffered_row_visibility_commit_held(
    commit: &crate::backend::blocked_users::VisibilityCommit,
    channel: &str,
    message_id: &str,
) {
    if !crate::backend::blocked_users::defer_channel_row_visibility_commit_held(
        commit, channel, message_id,
    ) {
        crate::log_irc!(
            error,
            "Blocked users: Pending channel-row admission raced despite the visibility commit gate"
        );
    }
}

#[derive(Default)]
pub(crate) struct BlockedVisibilityEffects {
    pub(super) channels: HashSet<String>,
    pub(super) whispers: Vec<(
        crate::backend::blocked_users::PendingWhisper,
        crate::backend::whispers::WhisperPublication,
    )>,
}

/// Complete the initial visibility transaction after either an authoritative
/// membership publication or the deliberate Failed/fail-open state. The
/// caller owns the visibility commit gate. External observer/sound work is
/// returned so it can run only after the gate is released.
pub(crate) fn finalize_pending_blocked_visibility_commit_held(
    commit: &crate::backend::blocked_users::VisibilityCommit,
    auth_epoch: u64,
) -> BlockedVisibilityEffects {
    let mut channels = apply_blocked_user_filter_to_buffers_visibility_commit_held(commit);
    let pending =
        crate::backend::blocked_users::take_pending_visibility_commit_held(commit, auth_epoch);

    channels.extend(
        pending
            .channel_rows
            .into_iter()
            .map(|row| row.channel.to_lowercase()),
    );
    if pending.reveal_resident_rows {
        channels.extend(CHAT_MESSAGES.iter().map(|entry| entry.key().clone()));
    }
    mark_blocked_channel_refresh_handoff(&channels);
    if pending.reveal_resident_rows {
        crate::backend::blocked_users::release_resident_visibility_commit_held(commit);
        for channel in &channels {
            seed_chatters_from_visible_buffer(channel);
        }
    }
    let whispers = pending
        .whispers
        .into_iter()
        .map(|whisper| {
            let publication = crate::backend::whispers::record_received_visibility_commit_held(
                commit,
                whisper.message.clone(),
            );
            (whisper, publication)
        })
        .collect();

    BlockedVisibilityEffects { channels, whispers }
}

pub(crate) fn dispatch_blocked_visibility_effects(effects: BlockedVisibilityEffects) {
    crate::backend::activity_persistence::hydrate_current_account();
    for channel in &effects.channels {
        crate::backend::history_cache::schedule_visible_channel_history_persist(channel);
    }
    dispatch_blocked_channel_refreshes(effects.channels);
    if !effects.whispers.is_empty() {
        crate::state::ui_invalidation::request(
            crate::state::ui_invalidation::UiInvalidation::TopBarState,
        );
    }
    for (whisper, publication) in effects.whispers {
        crate::backend::whispers::dispatch_publication(publication);
        crate::log_irc!(
            debug,
            "Whisper from {} released after blocked-user visibility became authoritative",
            whisper.sender_login
        );
        SOUND_STORE.play_sound(SoundEventType::Whisper);
        notify_whisper(&whisper.message.display_name, &whisper.message.content);
    }
}

/// Prune one channel against a stable immutable policy. The policy and channel
/// role are captured before any chat/UI lock, then buffers are reconciled in
/// place without cloning complete ChatMessage rows.
pub(super) fn apply_blocked_user_filter_to_channel_with_policy(
    channel: &str,
    policy: &crate::backend::blocked_users::BlockedVisibilityPolicy,
) -> bool {
    if policy.is_empty() {
        return false;
    }

    let channel = channel.trim_start_matches('#').to_lowercase();
    if channel.is_empty() {
        return false;
    }
    let hide_blocked_senders =
        !blocked_visibility_role_state_for(&channel, policy.visibility).unwrap_or(false);

    let result =
        CHAT_MESSAGES
            .get_mut(&channel)
            .map(|mut buffer| {
                buffer.reconcile(|message| {
                    if hide_blocked_senders
                        && is_user_attributed_message(message)
                        && policy.contains(message.user_id.as_deref(), message.sender.as_ref())
                    {
                        return MessageReconcileAction::Remove;
                    }
                    if message.reply.as_ref().is_some_and(|reply| {
                        policy.contains(None, reply.parent_user_login.as_ref())
                    }) {
                        message.reply = None;
                        return MessageReconcileAction::Mutated;
                    }
                    MessageReconcileAction::Keep
                })
            })
            .unwrap_or_default();

    if hide_blocked_senders && let Some(mut chatters) = RECENT_CHATTERS.get_mut(&channel) {
        let blocked: Vec<String> = chatters
            .iter()
            .filter(|(login, _)| policy.contains(None, login))
            .map(|(login, _)| login.clone())
            .collect();
        for login in blocked {
            chatters.pop(login.as_str());
        }
    }

    let mut mentions_changed = false;
    {
        let mut mentions = GLOBAL_MENTIONS.write();
        let before = mentions.len();
        mentions.retain_mut(|(mention_channel, message)| {
            if !mention_channel.eq_ignore_ascii_case(&channel) {
                return true;
            }
            if hide_blocked_senders
                && policy.contains(message.user_id.as_deref(), message.sender.as_ref())
            {
                return false;
            }
            if message
                .reply
                .as_ref()
                .is_some_and(|reply| policy.contains(None, reply.parent_user_login.as_ref()))
            {
                message.reply = None;
                mentions_changed = true;
            }
            true
        });
        mentions_changed |= mentions.len() != before;
    }

    if mentions_changed {
        reconcile_global_mention_unread();
    }

    if result.removed > 0 || result.mutated > 0 {
        crate::log_irc!(
            debug,
            "Blocked users: Pruned {} row(s) and redacted {} reply preview(s) from #{}",
            result.removed,
            result.mutated,
            channel
        );
    }
    if mentions_changed {
        crate::state::ui_invalidation::request(
            crate::state::ui_invalidation::UiInvalidation::TopBarState,
        );
    }

    result.removed > 0 || result.mutated > 0
}

/// Read-only precursor to the in-place reconciliation. The caller holds the
/// visibility commit gate, so the subsequent mutation observes the same
/// bounded message buffer and role policy.
pub(super) fn blocked_user_filter_would_change_channel(
    channel: &str,
    policy: &crate::backend::blocked_users::BlockedVisibilityPolicy,
) -> bool {
    if policy.is_empty() {
        return false;
    }

    let channel = channel.trim_start_matches('#').to_lowercase();
    if channel.is_empty() {
        return false;
    }
    let hide_blocked_senders =
        !blocked_visibility_role_state_for(&channel, policy.visibility).unwrap_or(false);

    CHAT_MESSAGES.get(&channel).is_some_and(|buffer| {
        buffer.iter().any(|message| {
            (hide_blocked_senders
                && is_user_attributed_message(message)
                && policy.contains(message.user_id.as_deref(), message.sender.as_ref()))
                || message
                    .reply
                    .as_ref()
                    .is_some_and(|reply| policy.contains(None, reply.parent_user_login.as_ref()))
        })
    })
}

pub(super) fn apply_blocked_user_filter_to_channel(channel: &str) -> bool {
    let Some(policy) = crate::backend::blocked_users::visibility_policy_snapshot() else {
        return false;
    };
    let expected_changed = blocked_user_filter_would_change_channel(channel, &policy);
    if expected_changed {
        mark_blocked_channel_refresh_handoff(&HashSet::from([channel.to_lowercase()]));
    }
    let changed = apply_blocked_user_filter_to_channel_with_policy(channel, &policy);
    if changed && !expected_changed {
        mark_blocked_channel_refresh_handoff(&HashSet::from([channel.to_lowercase()]));
    }
    changed || expected_changed
}

/// Whether a row speaks for a user, as opposed to a system notice.
pub(crate) fn is_user_attributed_message(message: &ChatMessage) -> bool {
    !message.flags.is_system
        || message.user_id.is_some()
        || !message.sender.eq_ignore_ascii_case("system")
}
