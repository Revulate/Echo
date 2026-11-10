//! Echo `ChatMessage` adapter for message search.

pub use super::search_core::{SearchMessage, compile_search_query, compile_search_query_with_case};

use super::models::ChatMessage;
use super::slots::{ResolvedSlot, SlotResolver};
use crate::backend::lock_utils::RwLockExt;
use rustc_hash::{FxHashMap, FxHashSet};
use std::borrow::Cow;
use std::sync::{LazyLock, RwLock};

#[derive(Default)]
struct ActiveSearchMarkers {
    version: u64,
    by_channel: FxHashMap<String, FxHashSet<String>>,
}

static ACTIVE_SEARCH_MARKERS: LazyLock<RwLock<ActiveSearchMarkers>> =
    LazyLock::new(|| RwLock::new(ActiveSearchMarkers::default()));

/// Replace the active search-result marker set shown in chat scroll maps.
///
/// Search results are retained after the modal closes so the user can still see
/// where the last search matched while navigating the landed channel.
pub fn set_active_search_markers(results: &[ChatMessage]) {
    let mut next_by_channel: FxHashMap<String, FxHashSet<String>> = FxHashMap::default();

    for msg in results {
        next_by_channel
            .entry(msg.channel.to_string())
            .or_default()
            .insert(msg.id.to_string());
    }

    let mut state = ACTIVE_SEARCH_MARKERS.write_or_recover();
    state.by_channel = next_by_channel;
    state.version = state.version.wrapping_add(1);
}

/// Clear active search-result markers from chat scroll maps.
pub fn clear_active_search_markers() {
    let mut state = ACTIVE_SEARCH_MARKERS.write_or_recover();
    if state.by_channel.is_empty() {
        return;
    }
    state.by_channel.clear();
    state.version = state.version.wrapping_add(1);
}

/// Monotonic version for active search-result scroll-map markers.
pub fn active_search_markers_version() -> u64 {
    ACTIVE_SEARCH_MARKERS.read_or_recover().version
}

/// Snapshot active search-result message IDs for one channel.
pub fn active_search_marker_ids(channel: &str) -> FxHashSet<String> {
    ACTIVE_SEARCH_MARKERS
        .read_or_recover()
        .by_channel
        .get(channel)
        .cloned()
        .unwrap_or_default()
}

pub fn resolved_search_text(msg: &ChatMessage) -> Option<String> {
    if msg.flags.is_system || msg.slots.is_empty() {
        return None;
    }

    let resolver = SlotResolver::new(msg.channel.as_ref());
    let text = msg
        .slots
        .iter()
        .map(|slot| match resolver.resolve(slot) {
            ResolvedSlot::Text(text) => text.to_string(),
            ResolvedSlot::Emote(emote) => emote.name.to_string(),
            ResolvedSlot::Url { url, .. } => url.to_string(),
            ResolvedSlot::Mention(username) => format!("@{}", username),
            ResolvedSlot::Emoji { emoji, .. } => emoji.to_string(),
            ResolvedSlot::TwitchGif { alt_text, .. } => alt_text.to_string(),
        })
        .collect::<Vec<_>>()
        .join(" ");

    if text.trim().is_empty() {
        None
    } else {
        Some(text)
    }
}

/// Root search adapter that preserves resolver-backed searchable text while
/// delegating every other predicate to the canonical core message model.
pub struct ResolvedSearchMessage<'a> {
    message: &'a ChatMessage,
    searchable_text: Option<String>,
}

impl<'a> ResolvedSearchMessage<'a> {
    pub fn new(message: &'a ChatMessage) -> Self {
        Self {
            message,
            searchable_text: resolved_search_text(message),
        }
    }
}

impl SearchMessage for ResolvedSearchMessage<'_> {
    fn content(&self) -> &str {
        self.message.content()
    }

    fn searchable_text(&self) -> Option<Cow<'_, str>> {
        self.searchable_text.as_deref().map(Cow::Borrowed)
    }

    fn sender(&self) -> &str {
        self.message.sender()
    }

    fn display_name(&self) -> &str {
        self.message.display_name()
    }

    fn channel(&self) -> &str {
        self.message.channel()
    }

    fn has_link(&self) -> bool {
        self.message.has_link()
    }

    fn has_emote(&self) -> bool {
        self.message.has_emote()
    }

    fn has_emoji(&self) -> bool {
        self.message.has_emoji()
    }

    fn has_mention(&self) -> bool {
        self.message.has_mention()
    }

    fn has_any_badge(&self) -> bool {
        self.message.has_any_badge()
    }

    fn has_badge(&self, badge: &str) -> bool {
        self.message.has_badge(badge)
    }

    fn has_subtier(&self, tier: &str) -> bool {
        self.message.has_subtier(tier)
    }

    fn has_type(&self, value: &str) -> bool {
        self.message.has_type(value)
    }
}
