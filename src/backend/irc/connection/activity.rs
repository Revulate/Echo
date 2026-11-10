//! Account-mention matching, the global mentions feed and per-channel tab activity.

use super::*;

pub(super) struct AccountMentionMatcher {
    pub(super) login: String,
    pub(super) display_name: String,
    pub(super) pattern: Option<regex::Regex>,
    /// When the display name was last read from settings; this runs for every
    /// message, so the settings lock is taken at most once per interval.
    pub(super) checked_at: std::time::Instant,
}

pub(super) const ACCOUNT_MENTION_NAME_RECHECK: std::time::Duration =
    std::time::Duration::from_secs(30);

impl AccountMentionMatcher {
    pub(super) fn new(login: &str, display_name: &str) -> Self {
        let mut names = regex::escape(login);
        if !display_name.is_empty() && !display_name.eq_ignore_ascii_case(login) {
            names.push('|');
            names.push_str(&regex::escape(display_name));
        }
        let pattern = (!login.is_empty())
            .then(|| {
                regex::RegexBuilder::new(&format!(r"\b(?:{names})\b"))
                    .case_insensitive(true)
                    .unicode(true)
                    .build()
                    .ok()
            })
            .flatten();
        Self {
            login: login.to_string(),
            display_name: display_name.to_string(),
            pattern,
            checked_at: std::time::Instant::now(),
        }
    }

    pub(super) fn is_match(&self, text: &str) -> bool {
        self.pattern
            .as_ref()
            .is_some_and(|pattern| pattern.is_match(text))
    }
}

pub(super) static ACCOUNT_MENTION_MATCHER: LazyLock<RwLock<Option<AccountMentionMatcher>>> =
    LazyLock::new(|| RwLock::new(None));

pub(crate) fn contains_account_mention(text: &str, login: &str) -> bool {
    use crate::backend::lock_utils::MutexExt;

    if login.is_empty() {
        return false;
    }
    {
        let cache = ACCOUNT_MENTION_MATCHER.read();
        if let Some(matcher) = cache.as_ref()
            && matcher.login == login
            && matcher.checked_at.elapsed() < ACCOUNT_MENTION_NAME_RECHECK
        {
            return matcher.is_match(text);
        }
    }
    let display_name = crate::backend::settings::SETTINGS
        .lock_or_recover()
        .accounts
        .iter()
        .find(|account| account.is_active && account.username.eq_ignore_ascii_case(login))
        .and_then(|account| account.display_name.as_deref())
        .unwrap_or_default()
        .trim()
        .to_string();
    {
        let mut cache = ACCOUNT_MENTION_MATCHER.write();
        if let Some(matcher) = cache.as_mut()
            && matcher.login == login
            && matcher.display_name == display_name
        {
            matcher.checked_at = std::time::Instant::now();
            return matcher.is_match(text);
        }
    }
    let matcher = AccountMentionMatcher::new(login, &display_name);
    let matched = matcher.is_match(text);
    *ACCOUNT_MENTION_MATCHER.write() = Some(matcher);
    matched
}

pub(super) const MAX_ACTIVITY_MENTIONS: usize = 500;

pub static GLOBAL_MENTIONS: LazyLock<RwLock<VecDeque<(String, ChatMessage)>>> =
    LazyLock::new(|| RwLock::new(VecDeque::with_capacity(MAX_ACTIVITY_MENTIONS)));

/// Unread keys are kept separately from the bounded Activity history so
/// observing a mention can clear attention without deleting its useful row.
pub(super) static GLOBAL_UNREAD_MENTIONS: LazyLock<RwLock<HashSet<(String, String)>>> =
    LazyLock::new(|| RwLock::new(HashSet::with_capacity(MAX_ACTIVITY_MENTIONS)));

pub(super) fn global_mention_key(channel: &str, message_id: &str) -> (String, String) {
    (
        channel.trim_start_matches('#').to_lowercase(),
        message_id.to_string(),
    )
}

pub(crate) fn mark_global_mention_unread(channel: &str, message_id: &str) {
    if message_id.is_empty() {
        return;
    }
    GLOBAL_UNREAD_MENTIONS
        .write()
        .insert(global_mention_key(channel, message_id));
    crate::backend::activity_persistence::mark_dirty();
}

pub(crate) fn remove_global_mention_unread(channel: &str, message_id: &str) -> bool {
    let changed = GLOBAL_UNREAD_MENTIONS
        .write()
        .remove(&global_mention_key(channel, message_id));
    // Removing a retained row also calls this when it was already read.
    crate::backend::activity_persistence::mark_dirty();
    changed
}

pub(crate) fn clear_global_mention_unread() -> bool {
    let mut unread = GLOBAL_UNREAD_MENTIONS.write();
    let changed = !unread.is_empty();
    unread.clear();
    crate::backend::activity_persistence::mark_dirty();
    changed
}

pub(crate) fn unread_global_mention_count() -> usize {
    GLOBAL_UNREAD_MENTIONS.read().len()
}

/// Mark only mention rows the canvas has proved are inside a visible viewport.
pub(crate) fn mark_visible_global_mentions_read(visible: &HashSet<(String, String)>) -> bool {
    if visible.is_empty() {
        return false;
    }
    let mut unread = GLOBAL_UNREAD_MENTIONS.write();
    let before = unread.len();
    unread.retain(|key| !visible.contains(key));
    let changed = before != unread.len();
    if changed {
        crate::backend::activity_persistence::mark_dirty();
    }
    changed
}

/// Drop unread keys whose retained Activity row was filtered or evicted.
pub(crate) fn reconcile_global_mention_unread() {
    let retained: HashSet<_> = GLOBAL_MENTIONS
        .read()
        .iter()
        .map(|(channel, message)| global_mention_key(channel, message.id.as_ref()))
        .collect();
    GLOBAL_UNREAD_MENTIONS
        .write()
        .retain(|key| retained.contains(key));
    crate::backend::activity_persistence::mark_dirty();
}

pub(crate) fn activity_mentions_snapshot_visibility_commit_held(
    _commit: &crate::backend::blocked_users::VisibilityCommit,
) -> Vec<(String, ChatMessage, bool)> {
    let mentions = GLOBAL_MENTIONS.read();
    let unread = GLOBAL_UNREAD_MENTIONS.read();
    mentions
        .iter()
        .skip(mentions.len().saturating_sub(MAX_ACTIVITY_MENTIONS))
        .map(|(channel, message)| {
            let is_unread = unread.contains(&global_mention_key(channel, message.id.as_ref()));
            (channel.clone(), message.clone(), is_unread)
        })
        .collect()
}

pub(crate) fn clear_activity_mentions_visibility_commit_held(
    _commit: &crate::backend::blocked_users::VisibilityCommit,
) {
    GLOBAL_MENTIONS.write().clear();
    GLOBAL_UNREAD_MENTIONS.write().clear();
    crate::state::ui_invalidation::request(
        crate::state::ui_invalidation::UiInvalidation::TopBarState,
    );
}

pub(crate) fn restore_activity_mentions_visibility_commit_held(
    _commit: &crate::backend::blocked_users::VisibilityCommit,
    rows: Vec<(String, ChatMessage, bool)>,
) {
    let mut mentions = GLOBAL_MENTIONS.write();
    let mut unread_keys = GLOBAL_UNREAD_MENTIONS.write();
    let resident = mentions.iter().map(|(channel, message)| {
        let unread = unread_keys.contains(&global_mention_key(channel, message.id.as_ref()));
        (channel.clone(), message.clone(), unread)
    });
    let mut merged = HashMap::new();
    for (channel, message, unread) in rows.into_iter().chain(resident) {
        if message.id.is_empty()
            || crate::backend::blocked_users::is_blocked(
                message.user_id.as_deref(),
                message.sender.as_ref(),
            )
        {
            continue;
        }
        let key = global_mention_key(&channel, message.id.as_ref());
        merged.insert(key, (channel, message, unread));
    }
    let mut rows: Vec<_> = merged.into_values().collect();
    rows.sort_by(|a, b| {
        a.1.timestamp
            .cmp(&b.1.timestamp)
            .then_with(|| a.0.cmp(&b.0))
            .then_with(|| a.1.id.cmp(&b.1.id))
    });
    let discard = rows.len().saturating_sub(MAX_ACTIVITY_MENTIONS);
    mentions.clear();
    unread_keys.clear();
    for (channel, mut message, unread) in rows.into_iter().skip(discard) {
        if message.reply.as_ref().is_some_and(|reply| {
            crate::backend::blocked_users::is_blocked(None, reply.parent_user_login.as_ref())
        }) {
            message.reply = None;
        }
        if unread {
            unread_keys.insert(global_mention_key(&channel, message.id.as_ref()));
        }
        mentions.push_back((channel, message));
    }
}

/// Tab-activity flag: a new message arrived in the channel.
pub const TAB_ACTIVITY_MESSAGE: u8 = 0b01;
/// Tab-activity flag: a mention/highlight arrived in the channel.
pub const TAB_ACTIVITY_MENTION: u8 = 0b10;

pub(super) static CHANNEL_TAB_ACTIVITY: LazyLock<Mutex<FxHashMap<String, u8>>> =
    LazyLock::new(|| Mutex::new(FxHashMap::default()));

/// Record that a channel saw activity (message and/or mention) for tab coloring.
pub fn mark_channel_tab_activity(channel: &str, flags: u8) {
    if flags == 0 {
        return;
    }
    let mut map = CHANNEL_TAB_ACTIVITY.lock();
    *map.entry(channel.to_string()).or_insert(0) |= flags;
}

/// Take and clear all pending per-channel tab-activity flags.
pub fn drain_channel_tab_activity() -> Vec<(String, u8)> {
    let mut map = CHANNEL_TAB_ACTIVITY.lock();
    if map.is_empty() {
        return Vec::new();
    }
    std::mem::take(&mut *map).into_iter().collect()
}
