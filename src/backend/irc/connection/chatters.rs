//! Recent chatters per channel, for @-mention completion.

use super::*;

/// One recent chatter, keyed by lowercase login in [`RECENT_CHATTERS`].
pub(super) struct RecentChatter {
    pub(super) display_name: String,
    pub(super) color: Option<String>,
    /// Observed messages, used to rank mention completion.
    pub(super) message_count: u32,
}

/// Recent chatters per channel for @username completion, most recent first.
///
/// An LRU keyed by login, so recording a chatter on every PRIVMSG is a
/// hash lookup instead of a scan and shift of up to `RECENT_CHATTERS_MAX`
/// entries. It grows on demand and is trimmed to `RECENT_CHATTERS_MAX`.
pub(super) static RECENT_CHATTERS: LazyLock<DashMap<String, LruCache<String, RecentChatter>>> =
    LazyLock::new(DashMap::new);

/// A chatter seen in a channel this session.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ObservedChatter {
    /// Lowercase Twitch login.
    pub login: String,
    pub display_name: String,
    /// Chat name color as sent by Twitch.
    pub color: Option<String>,
}

pub(super) struct RankedChatterCandidate {
    pub(super) recency_rank: usize,
    pub(super) message_count: u32,
    pub(super) login: String,
    pub(super) display_name: String,
    pub(super) color: Option<String>,
}

/// Most messages first, then most recent, then by login. Logins are unique, so
/// this is a total order.
pub(super) fn compare_ranked_chatters(
    left: &RankedChatterCandidate,
    right: &RankedChatterCandidate,
) -> std::cmp::Ordering {
    right
        .message_count
        .cmp(&left.message_count)
        .then_with(|| left.recency_rank.cmp(&right.recency_rank))
        .then_with(|| left.login.cmp(&right.login))
}

#[inline]
pub(super) fn chatter_prefix_matches(value: &str, normalized_prefix: &str) -> bool {
    if normalized_prefix.is_ascii() {
        value
            .as_bytes()
            .get(..normalized_prefix.len())
            .is_some_and(|head| head.eq_ignore_ascii_case(normalized_prefix.as_bytes()))
    } else {
        value.to_lowercase().starts_with(normalized_prefix)
    }
}

pub(super) const RECENT_CHATTERS_MAX: usize = 2_000;

/// Track a chatter for username completion
/// Moves user to front of list if already present, otherwise adds them
pub fn track_chatter(channel: &str, username: &str, display_name: &str, color: Option<String>) {
    let username_lower = username.to_lowercase();
    let mut chatters = RECENT_CHATTERS
        .entry(channel.to_lowercase())
        .or_insert_with(LruCache::unbounded);

    if let Some(chatter) = chatters.get_mut(username_lower.as_str()) {
        if chatter.display_name != display_name {
            chatter.display_name = display_name.to_string();
        }
        chatter.color = color;
        chatter.message_count = chatter.message_count.saturating_add(1);
        return;
    }
    chatters.put(
        username_lower,
        RecentChatter {
            display_name: display_name.to_string(),
            color,
            message_count: 1,
        },
    );
    while chatters.len() > RECENT_CHATTERS_MAX {
        chatters.pop_lru();
    }
}

/// Up to `limit` chatters of a channel, most recent first.
pub fn get_recent_chatters(channel: &str, limit: usize) -> Vec<ObservedChatter> {
    if crate::backend::blocked_users::resident_visibility_revoked() {
        return Vec::new();
    }
    let channel_lower = channel.to_lowercase();

    RECENT_CHATTERS
        .get(&channel_lower)
        .map(|chatters| {
            chatters
                .iter()
                .take(limit)
                .map(|(login, chatter)| ObservedChatter {
                    login: login.clone(),
                    display_name: chatter.display_name.clone(),
                    color: chatter.color.clone(),
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Get prefix-matching completion chatters ranked by observed message
/// frequency, then LRU recency, then normalized login for a deterministic
/// final fallback.
///
/// The bounded visual popup requests only its visible row count. Inline Tab
/// may request the full matching set, but only after its two-character gate.
/// Both paths filter while the bounded resident index is borrowed, clone only
/// matches, and avoid copying the complete count map on every keystroke.
pub fn get_ranked_chatters(channel: &str, prefix: &str, limit: usize) -> Vec<ObservedChatter> {
    if limit == 0 || crate::backend::blocked_users::resident_visibility_revoked() {
        return Vec::new();
    }
    let channel_lower = channel.to_lowercase();
    let prefix = prefix.trim_start_matches('@').to_lowercase();
    let Some(chatters) = RECENT_CHATTERS.get(&channel_lower) else {
        return Vec::new();
    };
    let mut matches: Vec<RankedChatterCandidate> = chatters
        .iter()
        .enumerate()
        .filter(|(_, (login, chatter))| {
            prefix.is_empty()
                || login.starts_with(&prefix)
                || chatter_prefix_matches(&chatter.display_name, &prefix)
        })
        .map(|(recency_rank, (login, chatter))| RankedChatterCandidate {
            recency_rank,
            message_count: chatter.message_count,
            login: login.clone(),
            display_name: chatter.display_name.clone(),
            color: chatter.color.clone(),
        })
        .collect();
    if matches.len() > limit {
        matches.select_nth_unstable_by(limit - 1, compare_ranked_chatters);
        matches.truncate(limit);
    }
    matches.sort_by(compare_ranked_chatters);
    matches
        .into_iter()
        .map(|candidate| ObservedChatter {
            login: candidate.login,
            display_name: candidate.display_name,
            color: candidate.color,
        })
        .collect()
}

pub(crate) fn clear_recent_chatters_for_benchmark(channel: &str) -> bool {
    if !crate::bench::ui_fixture::is_enabled() {
        return false;
    }
    let channel = channel.to_lowercase();
    RECENT_CHATTERS.remove(&channel).is_some()
}
