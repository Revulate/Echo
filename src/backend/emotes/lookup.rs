//! Lock-free emote lookups for chat ingestion: per-message snapshots of the
//! published maps and the fast name and id lookups over them.

use super::*;

/// The provider catalogs used to tokenize one message. Capturing these once
/// keeps token resolution internally consistent and avoids an additional
/// registry/slot atomic load for every word in a chat message.
pub struct EmoteLookupSnapshot {
    /// The sender's 7TV personal emotes, for other chatters' messages.
    personal: Option<Arc<EmoteNameMap>>,
    channel: Option<Arc<EmoteNameMap>>,
    /// The signed-in account's personal and subscription emotes, for its own text.
    user: Option<Arc<EmoteNameMap>>,
    global: Arc<EmoteNameMap>,
}

/// Author of the text being tokenized. A sender's 7TV personal emotes apply
/// only to that sender's messages, and the signed-in account's own personal
/// and subscription emotes only to its own messages.
#[derive(Clone, Copy, Debug, Default)]
pub struct EmoteSender<'a> {
    pub user_id: Option<&'a str>,
    pub login: Option<&'a str>,
}

impl<'a> EmoteSender<'a> {
    /// Text with no chatter author, such as system rows.
    pub const NONE: Self = Self {
        user_id: None,
        login: None,
    };

    pub fn new(user_id: Option<&'a str>, login: &'a str) -> Self {
        Self {
            user_id: user_id.filter(|id| !id.is_empty()),
            login: Some(login).filter(|login| !login.is_empty()),
        }
    }

    fn is_signed_in_account(&self) -> bool {
        self.login
            .is_some_and(crate::backend::settings::is_signed_in_username)
            || self
                .user_id
                .is_some_and(crate::backend::auth::is_active_authenticated_user_id)
    }
}

/// Capture the lookup catalogs for composer resolution, which is always the
/// signed-in account's own text. The channel key is normalized here so the
/// snapshot preserves the defensive behavior of the public lookup wrappers.
pub fn snapshot_emote_lookups(channel: &str) -> EmoteLookupSnapshot {
    let mut snapshot = snapshot_emote_lookups_for_sender(channel, EmoteSender::NONE);
    snapshot.user = Some(USER_EMOTES_LOOKUP.load_full());
    snapshot
}

/// Capture the lookup catalogs for one message written by `sender`.
pub fn snapshot_emote_lookups_for_sender(
    channel: &str,
    sender: EmoteSender<'_>,
) -> EmoteLookupSnapshot {
    let channel_key = normalize_channel_key(channel);
    let channel = channel_emote_publication_if_registered(&channel_key)
        .map(|publication| publication.lookup_snapshot());
    let (personal, user) = if sender.user_id.is_none() && sender.login.is_none() {
        (None, None)
    } else if sender.is_signed_in_account() {
        (None, Some(USER_EMOTES_LOOKUP.load_full()))
    } else {
        (sender.user_id.and_then(personal::lookup_for_user), None)
    };
    EmoteLookupSnapshot {
        personal,
        channel,
        user,
        global: GLOBAL_EMOTES_LOOKUP.load_full(),
    }
}

/// Get emote by name from a previously captured lookup snapshot, permitting a
/// case-folded fallback for non-tokenizer callers.
pub fn get_emote_fast_lowercased_from_snapshot(
    snapshot: &EmoteLookupSnapshot,
    name: &str,
) -> Option<Arc<Emote>> {
    let folded_name = if name.bytes().any(|b| b.is_ascii_uppercase()) {
        Some(name.to_ascii_lowercase())
    } else {
        None
    };

    if let Some(lookup) = snapshot.channel.as_deref() {
        if let Some(emote) = lookup.get(name)
            && emote_visible(emote)
        {
            return Some(Arc::clone(emote));
        }
        if let Some(ref folded) = folded_name
            && let Some(emote) = lookup.get(folded.as_str())
            && emote_visible(emote)
        {
            return Some(Arc::clone(emote));
        }
    }

    if let Some(user) = snapshot.user.as_deref() {
        if let Some(emote) = user.get(name)
            && emote_visible(emote)
        {
            return Some(Arc::clone(emote));
        }
        if let Some(ref folded) = folded_name
            && let Some(emote) = user.get(folded.as_str())
            && emote_visible(emote)
        {
            return Some(Arc::clone(emote));
        }
    }

    if let Some(emote) = snapshot.global.get(name)
        && emote_visible(emote)
    {
        return Some(Arc::clone(emote));
    }
    if let Some(ref folded) = folded_name
        && let Some(emote) = snapshot.global.get(folded.as_str())
        && emote_visible(emote)
    {
        return Some(Arc::clone(emote));
    }

    None
}

/// Get emote by name with a channel key. Takes a snapshot for this one
/// lookup; message tokenizers should pass one snapshot to the
/// `_from_snapshot` variants instead.
pub fn get_emote_fast_lowercased(channel_key_lowercase: &str, name: &str) -> Option<Arc<Emote>> {
    get_emote_fast_lowercased_from_snapshot(&snapshot_emote_lookups(channel_key_lowercase), name)
}

/// Get emote by exact case-sensitive name with pre-normalized channel key.
///
/// Unlike `get_emote_fast_lowercased`, this never falls back to case-folded
/// lookups. Chat tokenization should use this path to preserve emote
/// case-sensitivity semantics.
pub fn get_emote_fast_case_sensitive_from_snapshot(
    snapshot: &EmoteLookupSnapshot,
    name: &str,
) -> Option<Arc<Emote>> {
    let exact = |lookup: &EmoteNameMap| {
        lookup
            .get(name)
            .filter(|emote| emote.name.as_ref() == name && emote_visible(emote))
            .cloned()
    };
    if let Some(emote) = snapshot.personal.as_deref().and_then(exact) {
        return Some(emote);
    }
    if let Some(emote) = snapshot.channel.as_deref().and_then(exact) {
        return Some(emote);
    }
    if let Some(emote) = snapshot.user.as_deref().and_then(exact) {
        return Some(emote);
    }

    if let Some(emote) = snapshot.global.get(name)
        && emote.name.as_ref() == name
        && emote_visible(emote)
    {
        return Some(Arc::clone(emote));
    }

    None
}

/// Exact case-sensitive compatibility wrapper for one-off consumers.
pub fn get_emote_fast_case_sensitive(
    channel_key_lowercase: &str,
    name: &str,
) -> Option<Arc<Emote>> {
    get_emote_fast_case_sensitive_from_snapshot(
        &snapshot_emote_lookups(channel_key_lowercase),
        name,
    )
}

/// Get a Twitch emote by exact Twitch emote ID using cache ID indices.
///
/// Lookup order mirrors name-based resolution precedence:
/// channel -> user emotes -> global.
pub fn get_twitch_emote_by_id_fast(
    channel_key_lowercase: &str,
    twitch_emote_id: &str,
) -> Option<Arc<Emote>> {
    if let Some(entry) = EMOTE_CACHE.channels.get(channel_key_lowercase)
        && let Some(emote) = entry
            .emotes_by_id
            .get(twitch_emote_id, EmoteProvider::Twitch)
    {
        return Some(Arc::clone(emote));
    }

    let needs_normalize = channel_key_lowercase
        .bytes()
        .any(|b| b == b'#' || b.is_ascii_uppercase());
    if needs_normalize {
        let normalized = normalize_channel_key(channel_key_lowercase);
        if normalized != channel_key_lowercase
            && let Some(entry) = EMOTE_CACHE.channels.get(&normalized)
            && let Some(emote) = entry
                .emotes_by_id
                .get(twitch_emote_id, EmoteProvider::Twitch)
        {
            return Some(Arc::clone(emote));
        }
    }

    let user = EMOTE_CACHE.user_emotes.read_or_recover();
    if let Some(entry) = user.as_ref()
        && let Some(emote) = entry
            .emotes_by_id
            .get(twitch_emote_id, EmoteProvider::Twitch)
    {
        return Some(Arc::clone(emote));
    }
    drop(user);

    let global = EMOTE_CACHE.global.read_or_recover();
    if let Some(entry) = global.as_ref()
        && let Some(emote) = entry
            .emotes_by_id
            .get(twitch_emote_id, EmoteProvider::Twitch)
    {
        return Some(Arc::clone(emote));
    }

    None
}

/// Get a Twitch emote by exact ID from any cached Helix-backed source (channel/user/global).
///
/// This is useful for tooltip enrichment when the render path only has a reduced emote instance
/// but we want to recover exact owner/type metadata from the same cache data the emote picker uses.
pub fn get_twitch_emote_by_id_any_cached(twitch_emote_id: &str) -> Option<Arc<Emote>> {
    let user = EMOTE_CACHE.user_emotes.read_or_recover();
    if let Some(entry) = user.as_ref()
        && let Some(emote) = entry
            .emotes_by_id
            .get(twitch_emote_id, EmoteProvider::Twitch)
    {
        return Some(Arc::clone(emote));
    }
    drop(user);

    for entry in EMOTE_CACHE.channels.iter() {
        if let Some(emote) = entry
            .value()
            .emotes_by_id
            .get(twitch_emote_id, EmoteProvider::Twitch)
        {
            return Some(Arc::clone(emote));
        }
    }

    let global = EMOTE_CACHE.global.read_or_recover();
    if let Some(entry) = global.as_ref()
        && let Some(emote) = entry
            .emotes_by_id
            .get(twitch_emote_id, EmoteProvider::Twitch)
    {
        return Some(Arc::clone(emote));
    }

    None
}

/// Get all emotes for a channel from dual-index (Arc references for O(1) cloning)
pub fn get_channel_emotes_fast(channel_login: &str) -> Vec<Arc<Emote>> {
    let cache_key = normalize_channel_key(channel_login);
    EMOTE_BY_CHANNEL
        .read_or_recover()
        .get(&cache_key)
        .map(|emotes| {
            emotes
                .iter()
                .filter(|emote| emote_visible(emote))
                .map(Arc::clone)
                .collect()
        })
        .unwrap_or_default()
}
