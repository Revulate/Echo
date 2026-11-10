//! Shared core logic for emote picker components
//!
//! This module contains common types, helpers, and logic shared between
//! the inline emote picker and the popout window picker.

use crate::backend::emote_quality::{
    EmoteImageScale, NOMINAL_ONE_X_EMOTE_PX, resolve_auto_tier, select_emote_display_url,
};
use crate::backend::{self, Emote, EmoteCategory, EmoteProvider};
use crate::state::emote_preferences::{get_preferences, get_preferences_mut};
use crate::theme::colors;
use crate::utils::emote_provider_color;
use emojis::Group;
use gpui::{Hsla, SharedString};
use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;

/// Type alias for cached categories (Arc for cheap cloning)
pub type CachedCategories = Arc<Vec<EmoteCategory>>;

/// Number of recent emotes to show
pub const RECENT_EMOTES_COUNT: usize = 25;

/// Provider key used by persisted favorite, recent, and usage preferences.
pub(crate) fn provider_preference_key(provider: &EmoteProvider) -> &'static str {
    match provider {
        EmoteProvider::Twitch => "Twitch",
        EmoteProvider::SevenTV => "SevenTV",
        EmoteProvider::BTTV => "BTTV",
        EmoteProvider::FFZ => "FFZ",
        EmoteProvider::Emoji => "Emoji",
    }
}

#[inline]
pub(crate) fn emote_preference_key(emote: &Emote) -> String {
    format!("{}:{}", emote.id, provider_preference_key(&emote.provider))
}

/// Record a selected emote without coupling callers to picker state.
pub(crate) fn record_emote_usage(emote: &Emote) {
    if let Some(mut preferences) = get_preferences_mut() {
        preferences.record_usage(
            &emote.id,
            provider_preference_key(&emote.provider),
            &emote.name,
        );
        preferences.try_save();
    }
}

/// Base height for emote cells in the picker.
pub const EMOTE_PICKER_CELL_HEIGHT: f32 = 56.0;

/// Base width for square emote cells.
pub const EMOTE_PICKER_CELL_WIDTH: f32 = 56.0;

/// Padding inside cell (image is smaller than cell).
pub const EMOTE_PICKER_CELL_PADDING: f32 = 5.0;

/// Uniform row height used by picker virtualization.
pub const EMOTE_PICKER_ROW_HEIGHT: f32 = EMOTE_PICKER_CELL_HEIGHT + 4.0;

/// Number of emotes shown before the picker asks the user to intentionally load more.
///
/// This comfortably covers unusually large channel catalogs
/// while retaining an explicit guard for exceptional or malformed catalogs.
pub const DEFAULT_PICKER_PAGE_EMOTE_LIMIT: usize = 10_000;

/// Highest picker catalog capacity accepted through the debug/benchmark override.
pub const MAX_PICKER_PAGE_EMOTE_LIMIT: usize = 50_000;

/// Maximum aspect ratio for emotes (prevents extreme wide emotes from breaking layout)
pub const MAX_EMOTE_ASPECT_RATIO: f32 = 4.0;

/// Minimum stage duration to emit picker perf logs.
const PICKER_PERF_LOG_THRESHOLD_MS: u128 = 12;

#[inline]
fn record_picker_stage(stage: &str, started_at: Instant, items: usize) {
    let elapsed = started_at.elapsed();
    crate::backend::metrics::record_emote_stage(stage, elapsed, items);
    if elapsed.as_millis() >= PICKER_PERF_LOG_THRESHOLD_MS {
        crate::log_perf!(
            info,
            stage = stage,
            elapsed_ms = elapsed.as_millis(),
            items,
            "Emote picker stage"
        );
    } else {
        crate::log_perf!(
            trace,
            stage = stage,
            elapsed_ms = elapsed.as_millis(),
            items,
            "Emote picker stage"
        );
    }
}

fn emoji_group_id(group: Group) -> &'static str {
    match group {
        Group::SmileysAndEmotion => "smileys-emotion",
        Group::PeopleAndBody => "people-body",
        Group::AnimalsAndNature => "animals-nature",
        Group::FoodAndDrink => "food-drink",
        Group::TravelAndPlaces => "travel-places",
        Group::Activities => "activities",
        Group::Objects => "objects",
        Group::Symbols => "symbols",
        Group::Flags => "flags",
    }
}

fn emoji_group_label(group: Group) -> &'static str {
    match group {
        Group::SmileysAndEmotion => "Smileys & Emotion",
        Group::PeopleAndBody => "People & Body",
        Group::AnimalsAndNature => "Animals & Nature",
        Group::FoodAndDrink => "Food & Drink",
        Group::TravelAndPlaces => "Travel & Places",
        Group::Activities => "Activities",
        Group::Objects => "Objects",
        Group::Symbols => "Symbols",
        Group::Flags => "Flags",
    }
}

fn emoji_group_icon(group: Group) -> &'static str {
    match group {
        Group::SmileysAndEmotion => ":)",
        Group::PeopleAndBody => "P",
        Group::AnimalsAndNature => "An",
        Group::FoodAndDrink => "Fd",
        Group::TravelAndPlaces => "Tr",
        Group::Activities => "Ac",
        Group::Objects => "Ob",
        Group::Symbols => "Sy",
        Group::Flags => "Fl",
    }
}

pub(crate) fn build_picker_emoji_emotes(
    style: crate::backend::settings::EmojiStyle,
) -> Vec<Arc<Emote>> {
    let mut emotes = Vec::with_capacity(4000);

    for group in Group::iter() {
        for emoji in group.emojis() {
            let emoji_text = emoji.as_str();
            let shortcode = emoji.shortcode().unwrap_or_else(|| emoji.name());
            emotes.push(Arc::new(Emote {
                id: format!("emoji:{shortcode}:{}", emoji_text).into(),
                name: emoji_text.into(),
                original_name: Some(emoji.name().into()),
                url: crate::backend::emoji::get_emoji_url(emoji_text, &style).into(),
                provider: EmoteProvider::Emoji,
                channel: Some(format!("emoji:{}", emoji_group_id(group)).into()),
                url_1x: None,
                url_2x: None,
                url_4x: None,
                is_animated: false,
                width: Some(64),
                height: Some(64),
            }));
        }
    }

    emotes
}

/// Calculate display dimensions for an emote cell and image preserving aspect ratio
///
/// Picker cells constrain the preview envelope; chat uses native logical size.
/// Wide previews fit within the envelope without changing their aspect ratio.
///
/// Returns (cell_width, cell_height, image_width, image_height) in pixels
pub fn calculate_emote_dimensions(emote: &Emote) -> (f32, f32, f32, f32) {
    let image_height = EMOTE_PICKER_CELL_HEIGHT - EMOTE_PICKER_CELL_PADDING * 2.0;

    match (emote.width, emote.height) {
        (Some(w), Some(h)) if w > 0 && h > 0 => {
            let aspect = w as f32 / h as f32;
            let image_width = image_height * aspect.min(MAX_EMOTE_ASPECT_RATIO);
            let image_height = image_width / aspect;

            let cell_width =
                (image_width + EMOTE_PICKER_CELL_PADDING * 2.0).max(EMOTE_PICKER_CELL_WIDTH);

            (
                cell_width,
                EMOTE_PICKER_CELL_HEIGHT,
                image_width,
                image_height,
            )
        }
        _ => {
            let size = image_height;
            (
                EMOTE_PICKER_CELL_WIDTH,
                EMOTE_PICKER_CELL_HEIGHT,
                size,
                size,
            )
        }
    }
}

/// URL to use for picker rendering/prefetch.
///
/// Honor the user's configured ceiling without decoding more source pixels than
/// this compact surface can display. In particular, a global 4x preference
/// should not make a 46px picker cell retain a 112px source on a 100%-scale
/// display; chat, tooltips, and other larger surfaces still use the configured
/// quality unchanged.
pub fn picker_display_url(emote: &Emote) -> &str {
    let (_, _, _, image_height) = calculate_emote_dimensions(emote);
    emote_display_url_at_height(emote, image_height)
}

pub(crate) fn emote_display_url_at_height(emote: &Emote, image_height: f32) -> &str {
    let preferred_scale = crate::backend::settings::get_emote_image_scale();
    let native_height = emote
        .height
        .filter(|height| *height > 0)
        .map(|height| height as f32)
        .unwrap_or(NOMINAL_ONE_X_EMOTE_PX);
    let display_scale = resolve_auto_tier(
        crate::components::chat::canvas_renderer::layout_scale::current(),
        image_height / native_height,
    );
    let scale = if preferred_scale == EmoteImageScale::Auto
        || preferred_scale.as_u8() > display_scale.as_u8()
    {
        display_scale
    } else {
        preferred_scale
    };
    select_emote_display_url(
        emote.url.as_ref(),
        emote.url_1x.as_deref(),
        emote.url_2x.as_deref(),
        emote.url_4x.as_deref(),
        scale,
    )
}

#[derive(Hash, PartialEq, Eq)]
enum EmoteCatalogIdentity {
    Id(&'static str, Arc<str>),
    Fallback(&'static str, Arc<str>, Arc<str>),
}

fn emote_catalog_identity_key(emote: &Emote) -> EmoteCatalogIdentity {
    let provider = provider_preference_key(&emote.provider);
    if emote.id.is_empty() {
        EmoteCatalogIdentity::Fallback(provider, Arc::clone(&emote.name), Arc::clone(&emote.url))
    } else {
        EmoteCatalogIdentity::Id(provider, Arc::clone(&emote.id))
    }
}

/// Stable identity for retaining a keyboard selection across live catalog rebuilds.
///
/// Provider IDs are authoritative when present. Synthetic/fallback entries use
/// provider + channel + name so image URL refreshes cannot move the selection.
pub(crate) fn emote_selection_identity_key(emote: &Emote) -> String {
    if emote.id.is_empty() {
        format!(
            "{}\0fallback\0{}\0{}",
            provider_preference_key(&emote.provider),
            emote.channel.as_deref().unwrap_or_default(),
            emote.name
        )
    } else {
        format!(
            "{}\0id\0{}",
            provider_preference_key(&emote.provider),
            emote.id
        )
    }
}

/// Whether picker cells should let GPUI own native animated image playback.
///
/// Defaulting this off keeps large picker views on Echo's bounded animated
/// frame cache instead of GPUI's unbounded URL image retention. Animations
/// still play in the picker; this flag exists only for side-by-side debugging
/// of GPUI-native animation memory behavior.
pub fn picker_native_animated_enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| echo_core::env::flag("ECHO_PICKER_NATIVE_ANIMATED"))
}

pub fn picker_page_emote_limit() -> usize {
    echo_core::env::parsed::<usize>("ECHO_PICKER_PAGE_EMOTE_LIMIT")
        .unwrap_or(DEFAULT_PICKER_PAGE_EMOTE_LIMIT)
        .clamp(80, MAX_PICKER_PAGE_EMOTE_LIMIT)
}

pub fn picker_tooltip_text(emote: &Emote) -> String {
    let mut text = format!(
        "{} - {}",
        emote.name.trim(),
        provider_display_name(&emote.provider)
    );
    if let Some(scope) = picker_scope_label(emote) {
        text.push_str(" - ");
        text.push_str(&scope);
    }
    text
}

fn picker_scope_label(emote: &Emote) -> Option<String> {
    let channel = emote.channel.as_deref()?.trim();
    if channel.is_empty() {
        return None;
    }

    if channel == "personal:7tv" {
        return Some("Personal".to_string());
    }

    if let Some(group) = channel.strip_prefix("emoji:") {
        return Some(format!("Emoji {}", group.replace('-', " ")));
    }

    if let Some(channel) = channel.strip_prefix("sub:") {
        return Some(format!("Sub emotes: {}", channel.trim_start_matches('#')));
    }

    if let Some(category) = channel.strip_prefix("twitch:") {
        return Some(format!("Twitch {}", category.replace('-', " ")));
    }

    Some(format!("Channel {}", channel.trim_start_matches('#')))
}

/// Tab types for emote picker navigation (shared between inline and popout)
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum EmoteTab {
    /// User's favorite emotes
    Favorites,
    /// Recently used emotes
    Recent,
    /// Twitch subscription emotes (from subs, follows, bits, etc.)
    Subs,
    /// Channel-specific emotes (7TV, BTTV, FFZ for current channel)
    #[default]
    Channel,
    /// Global emotes (Twitch, 7TV, BTTV, FFZ globals)
    Global,
    /// Emoji (Twitter, Apple, Facebook styles)
    Emoji,
}

impl EmoteTab {
    /// Get icon character for this tab
    pub fn icon(&self) -> &'static str {
        match self {
            EmoteTab::Favorites => "★",
            EmoteTab::Recent => "🕐",
            EmoteTab::Subs => "💜",
            EmoteTab::Channel => "📺",
            EmoteTab::Global => "🌐",
            EmoteTab::Emoji => "😀",
        }
    }

    /// Get label for this tab
    pub fn label(&self) -> &'static str {
        match self {
            EmoteTab::Favorites => "Favorites",
            EmoteTab::Recent => "Recent",
            EmoteTab::Subs => "Subs",
            EmoteTab::Channel => "Channel",
            EmoteTab::Global => "Global",
            EmoteTab::Emoji => "Emoji",
        }
    }

    /// Standard tabs for inline picker
    pub fn inline_tabs() -> &'static [EmoteTab] {
        &[
            EmoteTab::Favorites,
            EmoteTab::Recent,
            EmoteTab::Subs,
            EmoteTab::Channel,
            EmoteTab::Global,
            EmoteTab::Emoji,
        ]
    }

    /// Extended tabs for popout picker (same for now)
    pub fn popout_tabs() -> &'static [EmoteTab] {
        &[
            EmoteTab::Favorites,
            EmoteTab::Recent,
            EmoteTab::Subs,
            EmoteTab::Channel,
            EmoteTab::Global,
            EmoteTab::Emoji,
        ]
    }
}

/// Category header state for rendering
#[derive(Clone, Debug)]
pub struct CategoryHeader {
    pub id: String,
    pub name: String,
    pub icon: String,
    /// Pre-formatted count string (avoids format!() allocation per frame)
    pub count_str: SharedString,
    pub is_collapsed: bool,
    pub is_special: bool,
    pub provider: Option<EmoteProvider>,
}

/// Grid item for rendering - either a header or emotes
#[derive(Clone, Debug)]
pub enum GridItem {
    Header(CategoryHeader),
    /// Range for a section in `EmotePickerCore::flat_emotes`.
    Emotes {
        range: std::ops::Range<usize>,
    },
}

/// Loading state for the emote picker
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoadingState {
    /// Emotes are being loaded in background
    Loading,
    /// Emotes have been loaded
    Loaded,
    /// No emotes found (empty after load)
    Empty,
}

/// Resolution scope used as the final semantic ranking preference.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum EmoteScope {
    Channel,
    Personal,
    Subscription,
    Global,
    Emoji,
}

impl EmoteScope {
    #[inline]
    fn priority(self) -> u8 {
        match self {
            Self::Channel => 0,
            Self::Personal => 1,
            Self::Subscription => 2,
            Self::Global => 3,
            Self::Emoji => 4,
        }
    }

    fn classify(emote: &Emote, active_channel: &str) -> Self {
        if emote.provider == EmoteProvider::Emoji {
            return Self::Emoji;
        }

        let Some(channel) = emote.channel.as_deref() else {
            return Self::Global;
        };
        let channel = channel.trim_start_matches('#').to_lowercase();
        if channel == "personal:7tv" {
            return Self::Personal;
        }
        if channel.starts_with("twitch:") {
            return Self::Global;
        }
        if !active_channel.is_empty() && channel == active_channel {
            return Self::Channel;
        }
        if channel.starts_with("sub:") || emote.provider == EmoteProvider::Twitch {
            return Self::Subscription;
        }
        Self::Channel
    }
}

/// Query relevance band. Lower priority always sorts before preference signals.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum EmoteSearchRelevance {
    ExactCode,
    ExactAlias,
    Prefix,
    WordStart,
    Substring,
    Subsequence,
    Empty,
}

impl EmoteSearchRelevance {
    #[inline]
    fn priority(self) -> u8 {
        match self {
            Self::ExactCode => 0,
            Self::ExactAlias => 1,
            Self::Prefix => 2,
            Self::WordStart => 3,
            Self::Substring => 4,
            Self::Subsequence => 5,
            Self::Empty => 6,
        }
    }
}

/// Ranked output shared by picker and autocomplete consumers.
#[derive(Clone, Debug)]
pub(crate) struct RankedEmote {
    pub(crate) emote: Arc<Emote>,
    pub(crate) fuzzy_score: f64,
    pub(crate) scope: EmoteScope,
    /// Whether the query exactly matches or prefixes this emote's code or an
    /// alias. Inline Tab completion is intentionally prefix-only.
    pub(crate) is_prefix_match: bool,
}

struct IndexedAlias {
    text: Arc<str>,
    casefolded: String,
}

struct EmoteSearchEntry {
    emote: Arc<Emote>,
    code_casefolded: String,
    aliases: Vec<IndexedAlias>,
    preference_key: String,
    scope: EmoteScope,
}

#[derive(Default)]
struct RankingPreferences {
    favorites: HashSet<String>,
    recent_ranks: HashMap<String, usize>,
    usage_counts: HashMap<String, u32>,
}

impl RankingPreferences {
    fn snapshot() -> Self {
        let preferences = get_preferences();
        let favorites = preferences.favorites_set().clone();
        let recent_ranks = preferences
            .data
            .recently_used
            .iter()
            .enumerate()
            .map(|(rank, emote)| (emote.key(), rank))
            .collect();
        let usage_counts = preferences.data.usage_counts.clone();
        Self {
            favorites,
            recent_ranks,
            usage_counts,
        }
    }
}

struct RankedCandidate {
    entry_index: usize,
    relevance: EmoteSearchRelevance,
    case_exact: bool,
    fuzzy_score: f64,
    is_favorite: bool,
    recent_rank: Option<usize>,
    usage_count: u32,
}

fn has_word_start_match(target: &str, target_casefolded: &str, query_casefolded: &str) -> bool {
    let mut previous = None;
    let mut casefolded_offset = 0;
    for original in target.chars() {
        if target_casefolded
            .get(casefolded_offset..)
            .is_some_and(|suffix| suffix.starts_with(query_casefolded))
            && previous.is_some_and(|previous: char| {
                !previous.is_alphanumeric()
                    || (previous.is_ascii_digit() && !original.is_ascii_digit())
                    || (previous.is_lowercase() && original.is_uppercase())
            })
        {
            return true;
        }
        previous = Some(original);
        casefolded_offset += original.to_lowercase().map(char::len_utf8).sum::<usize>();
    }
    false
}

/// Catalog-backed deterministic search index. A provider/ID identity is indexed
/// once while same-code emotes from different providers remain distinct.
#[derive(Default)]
pub(crate) struct EmoteSearchIndex {
    entries: Vec<EmoteSearchEntry>,
}

impl EmoteSearchIndex {
    pub(crate) fn new<I>(active_channel: &str, emotes: I) -> Self
    where
        I: IntoIterator<Item = Arc<Emote>>,
    {
        let mut index = Self::default();
        index.rebuild(active_channel, emotes);
        index
    }

    pub(crate) fn rebuild<I>(&mut self, active_channel: &str, emotes: I)
    where
        I: IntoIterator<Item = Arc<Emote>>,
    {
        self.entries.clear();
        let active_channel = echo_core::channel_identity::normalize_channel_key(active_channel);
        let mut identity_positions = HashMap::new();

        for emote in emotes {
            let preference_key = emote_preference_key(&emote);
            let catalog_identity = emote_catalog_identity_key(&emote);
            if let Some(&entry_index) = identity_positions.get(&catalog_identity) {
                let entry: &mut EmoteSearchEntry = &mut self.entries[entry_index];
                let candidate_scope = EmoteScope::classify(&emote, &active_channel);
                if candidate_scope.priority() < entry.scope.priority() {
                    entry.scope = candidate_scope;
                }
                Self::add_search_aliases(entry, &emote);
                continue;
            }

            let mut entry = EmoteSearchEntry {
                code_casefolded: emote.name.to_lowercase(),
                aliases: Vec::with_capacity(1),
                scope: EmoteScope::classify(&emote, &active_channel),
                preference_key: preference_key.clone(),
                emote,
            };
            let emote = Arc::clone(&entry.emote);
            Self::add_search_aliases(&mut entry, &emote);
            identity_positions.insert(catalog_identity, self.entries.len());
            self.entries.push(entry);
        }
    }

    pub(crate) fn rank(&self, query: &str) -> Vec<RankedEmote> {
        let preferences = RankingPreferences::snapshot();
        self.rank_with_preferences(query, &preferences, None, false)
    }

    pub(crate) fn rank_prefix(&self, query: &str) -> Vec<RankedEmote> {
        let preferences = RankingPreferences::snapshot();
        self.rank_with_preferences(query, &preferences, None, true)
    }

    /// The best `limit` results, in the same order `rank` would give them.
    pub(crate) fn rank_top(&self, query: &str, limit: usize) -> Vec<RankedEmote> {
        if limit >= self.entries.len() {
            return self.rank(query);
        }
        let preferences = RankingPreferences::snapshot();
        self.rank_with_preferences(query, &preferences, Some(limit), false)
    }

    /// Indexed emote whose code is exactly `name`. Several sets can hold the
    /// same code, so `image_url` (the suggestion's picker URL) picks the one
    /// that was shown, matched against every size so the window scale at
    /// accept time does not matter; without a match the first entry wins.
    pub(crate) fn find_code(&self, name: &str, image_url: Option<&str>) -> Option<&Arc<Emote>> {
        let mut same_code = self
            .entries
            .iter()
            .filter(|entry| entry.emote.name.as_ref() == name)
            .map(|entry| &entry.emote);
        let first = same_code.next()?;
        let Some(image_url) = image_url else {
            return Some(first);
        };
        let shows = |emote: &Emote| {
            emote.url.as_ref() == image_url
                || [&emote.url_1x, &emote.url_2x, &emote.url_4x]
                    .into_iter()
                    .any(|url| url.as_deref() == Some(image_url))
        };
        if shows(first) {
            return Some(first);
        }
        Some(same_code.find(|emote| shows(emote)).unwrap_or(first))
    }

    fn add_alias(entry: &mut EmoteSearchEntry, alias: Arc<str>) {
        if alias.trim().is_empty() {
            return;
        }
        let casefolded = alias.to_lowercase();
        if casefolded == entry.code_casefolded
            || entry
                .aliases
                .iter()
                .any(|existing| existing.casefolded == casefolded)
        {
            return;
        }
        entry.aliases.push(IndexedAlias {
            text: alias,
            casefolded,
        });
    }

    /// Add independent lookup aliases for an emote. Emoji metadata has a
    /// display name plus one or more GitHub shortcodes; treating all of those
    /// as a single string lets fuzzy matching bridge their separator and
    /// produce unrelated results.
    fn add_search_aliases(entry: &mut EmoteSearchEntry, emote: &Emote) {
        Self::add_alias(entry, Arc::clone(&emote.name));

        if emote.provider == EmoteProvider::Emoji
            && let Some(emoji) = emojis::get(&emote.name)
        {
            Self::add_alias(entry, Arc::from(emoji.name()));
            for shortcode in emoji.shortcodes() {
                Self::add_alias(entry, Arc::from(shortcode));
            }
            return;
        }

        if let Some(alias) = emote.original_name.as_ref() {
            Self::add_alias(entry, Arc::clone(alias));
        }
    }

    fn rank_with_preferences(
        &self,
        query: &str,
        preferences: &RankingPreferences,
        limit: Option<usize>,
        prefix_only: bool,
    ) -> Vec<RankedEmote> {
        let query = query.trim();
        let fuzzy_query = echo_core::fuzzy::FuzzyQuery::new(query);
        let query_casefolded = query.to_lowercase();
        let empty_query = query_casefolded.is_empty();
        let colon_query_casefolded = format!(":{query_casefolded}");
        let mut candidates = Vec::with_capacity(if prefix_only { 32 } else { self.entries.len() });

        for (entry_index, entry) in self.entries.iter().enumerate() {
            if prefix_only
                && (entry.emote.provider == EmoteProvider::Emoji
                    || !(entry.code_casefolded.starts_with(&query_casefolded)
                        || entry.code_casefolded == colon_query_casefolded
                        || entry
                            .aliases
                            .iter()
                            .any(|alias| alias.casefolded.starts_with(&query_casefolded))))
            {
                continue;
            }
            let (relevance, fuzzy_score) = if empty_query {
                (EmoteSearchRelevance::Empty, 0.0)
            } else if entry.code_casefolded == query_casefolded
                || entry.code_casefolded == colon_query_casefolded
            {
                (
                    EmoteSearchRelevance::ExactCode,
                    fuzzy_query
                        .score(&entry.emote.name, &entry.code_casefolded)
                        .unwrap_or(0.0),
                )
            } else if let Some(alias) = entry
                .aliases
                .iter()
                .find(|alias| alias.casefolded == query_casefolded)
            {
                (
                    EmoteSearchRelevance::ExactAlias,
                    fuzzy_query
                        .score(alias.text.as_ref(), &alias.casefolded)
                        .unwrap_or(0.0),
                )
            } else {
                let has_prefix = entry.code_casefolded.starts_with(&query_casefolded)
                    || entry
                        .aliases
                        .iter()
                        .any(|alias| alias.casefolded.starts_with(&query_casefolded));
                let has_word_start = has_word_start_match(
                    entry.emote.name.as_ref(),
                    &entry.code_casefolded,
                    &query_casefolded,
                ) || entry.aliases.iter().any(|alias| {
                    has_word_start_match(alias.text.as_ref(), &alias.casefolded, &query_casefolded)
                });
                let has_substring = entry.code_casefolded.contains(&query_casefolded)
                    || entry
                        .aliases
                        .iter()
                        .any(|alias| alias.casefolded.contains(&query_casefolded));
                let fuzzy_score =
                    std::iter::once((entry.emote.name.as_ref(), entry.code_casefolded.as_str()))
                        .chain(
                            entry
                                .aliases
                                .iter()
                                .map(|alias| (alias.text.as_ref(), alias.casefolded.as_str())),
                        )
                        .filter_map(|(target, target_lower)| {
                            fuzzy_query.score(target, target_lower)
                        })
                        .max_by(f64::total_cmp);
                let Some(fuzzy_score) = fuzzy_score else {
                    continue;
                };
                (
                    if has_prefix {
                        EmoteSearchRelevance::Prefix
                    } else if has_word_start {
                        EmoteSearchRelevance::WordStart
                    } else if has_substring {
                        EmoteSearchRelevance::Substring
                    } else {
                        EmoteSearchRelevance::Subsequence
                    },
                    fuzzy_score,
                )
            };

            candidates.push(RankedCandidate {
                entry_index,
                relevance,
                case_exact: relevance == EmoteSearchRelevance::ExactCode
                    && entry.emote.name.as_ref() == query,
                fuzzy_score,
                is_favorite: preferences.favorites.contains(&entry.preference_key),
                recent_rank: preferences.recent_ranks.get(&entry.preference_key).copied(),
                usage_count: preferences
                    .usage_counts
                    .get(&entry.preference_key)
                    .copied()
                    .unwrap_or(0),
            });
        }

        if !prefix_only
            && candidates.iter().any(|candidate| {
                candidate.relevance.priority() <= EmoteSearchRelevance::Substring.priority()
            })
        {
            candidates.retain(|candidate| candidate.relevance != EmoteSearchRelevance::Subsequence);
        }

        let compare = |a: &RankedCandidate, b: &RankedCandidate| {
            let entry_a = &self.entries[a.entry_index];
            let entry_b = &self.entries[b.entry_index];
            let relevance_order = if empty_query {
                Ordering::Equal
            } else {
                a.relevance.priority().cmp(&b.relevance.priority())
            };
            relevance_order
                .then_with(|| b.case_exact.cmp(&a.case_exact))
                .then_with(|| {
                    if empty_query {
                        Ordering::Equal
                    } else {
                        b.fuzzy_score.total_cmp(&a.fuzzy_score)
                    }
                })
                .then_with(|| b.is_favorite.cmp(&a.is_favorite))
                .then_with(|| {
                    a.recent_rank
                        .unwrap_or(usize::MAX)
                        .cmp(&b.recent_rank.unwrap_or(usize::MAX))
                })
                .then_with(|| b.usage_count.cmp(&a.usage_count))
                .then_with(|| entry_a.scope.priority().cmp(&entry_b.scope.priority()))
                .then_with(|| entry_a.code_casefolded.cmp(&entry_b.code_casefolded))
                .then_with(|| {
                    provider_preference_key(&entry_a.emote.provider)
                        .cmp(provider_preference_key(&entry_b.emote.provider))
                })
                .then_with(|| entry_a.emote.id.cmp(&entry_b.emote.id))
                .then_with(|| entry_a.emote.url.cmp(&entry_b.emote.url))
                .then_with(|| a.entry_index.cmp(&b.entry_index))
        };
        match limit {
            Some(limit) if limit < candidates.len() => {
                if limit == 0 {
                    candidates.clear();
                } else {
                    candidates.select_nth_unstable_by(limit - 1, compare);
                    candidates.truncate(limit);
                }
                candidates.sort_unstable_by(compare);
            }
            _ => candidates.sort_unstable_by(compare),
        }

        candidates
            .into_iter()
            .map(|candidate| {
                let entry = &self.entries[candidate.entry_index];
                RankedEmote {
                    emote: Arc::clone(&entry.emote),
                    fuzzy_score: candidate.fuzzy_score,
                    scope: entry.scope,
                    is_prefix_match: matches!(
                        candidate.relevance,
                        EmoteSearchRelevance::ExactCode
                            | EmoteSearchRelevance::ExactAlias
                            | EmoteSearchRelevance::Prefix
                    ),
                }
            })
            .collect()
    }
}

/// Catalog + search index transferred from one picker surface to the next.
///
/// The inline picker releases its catalog as soon as it closes, so the popout
/// can only reuse that work if the inline surface hands the `Arc`s over before
/// the release. The slot holds at most one pending transfer and is emptied by
/// the first surface that inspects it, so nothing is retained long term.
struct PickerCatalogHandoff {
    channel: String,
    categories: CachedCategories,
    emoji_style: crate::backend::settings::EmojiStyle,
    all_emotes: Arc<Vec<Arc<Emote>>>,
    search_index: Arc<EmoteSearchIndex>,
}

static PICKER_CATALOG_HANDOFF: Mutex<Option<PickerCatalogHandoff>> = Mutex::new(None);

/// The handoff slot. A panic while it was held cannot leave a half-written
/// handoff (every writer replaces the whole value), so a poisoned slot is used
/// as is rather than disabling the handoff for the rest of the session.
fn catalog_handoff_slot() -> std::sync::MutexGuard<'static, Option<PickerCatalogHandoff>> {
    PICKER_CATALOG_HANDOFF
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Drop a pending handoff so an aborted transfer cannot retain a catalog.
pub(crate) fn clear_picker_catalog_handoff() {
    *catalog_handoff_slot() = None;
}

/// Shared emote picker core with common state and logic
pub struct EmotePickerCore {
    pub channel: String,
    pub search_query: String,
    pub active_tab: EmoteTab,
    pub categories: CachedCategories,
    pub grid_items: Vec<GridItem>,
    pub flat_emotes: Vec<Arc<Emote>>,
    /// Flattened emotes cache to avoid rebuilding category flat-lists on every tab/search change.
    all_emotes_cache: Arc<Vec<Arc<Emote>>>,
    /// Alias-aware deterministic ranking index rebuilt with the catalog.
    /// Shared by `Arc` so a second picker surface can adopt it as-is.
    search_index: Arc<EmoteSearchIndex>,
    /// Emoji style used to generate the current synthetic emoji catalog.
    catalog_emoji_style: crate::backend::settings::EmojiStyle,
    last_channel_emote_version: u32,
    last_global_emote_version: u32,
    last_user_emote_version: u64,
    last_layout_settings_version: u64,
    cached_favorites: HashSet<String>,
    /// Lightweight signature of categories content to avoid expensive no-op refresh/rebuild.
    categories_signature: u64,
}

impl EmotePickerCore {
    /// Create a new core with the given channel
    pub fn new(channel: String) -> Self {
        Self {
            channel,
            search_query: String::new(),
            active_tab: EmoteTab::Channel,
            categories: Arc::new(vec![]),
            grid_items: Vec::new(),
            flat_emotes: Vec::new(),
            all_emotes_cache: Arc::new(Vec::new()),
            search_index: Arc::new(EmoteSearchIndex::default()),
            catalog_emoji_style: crate::backend::settings::get_emoji_style(),
            last_channel_emote_version: 0,
            last_global_emote_version: 0,
            last_user_emote_version: 0,
            last_layout_settings_version: 0,
            cached_favorites: HashSet::new(),
            categories_signature: 0,
        }
    }

    /// Release category, search, and row-builder data retained after the picker closes.
    ///
    /// Media bytes are scoped separately in the image cache. This clears the
    /// metadata/search side so opening a large picker does not permanently
    /// raise normal chat memory.
    pub fn clear_heavy_caches(&mut self) {
        self.search_query.clear();
        self.categories = Arc::new(Vec::new());
        self.grid_items.clear();
        self.flat_emotes.clear();
        self.all_emotes_cache = Arc::new(Vec::new());
        self.search_index = Arc::new(EmoteSearchIndex::default());
        self.cached_favorites.clear();
        self.categories_signature = 0;
        self.last_channel_emote_version = 0;
        self.last_global_emote_version = 0;
        self.last_user_emote_version = 0;
        self.last_layout_settings_version = 0;
    }

    /// Actually load emotes (call after construction)
    pub fn load_emotes(&mut self) {
        let total_start = Instant::now();
        let channel_version = if !self.channel.is_empty() {
            crate::backend::emotes::get_emote_cache_version(&self.channel)
        } else {
            0
        };
        let global_version = crate::backend::emotes::get_global_emote_version();
        let user_version = crate::state::VersionRegistry::user_emote_version();
        let layout_version = crate::backend::settings::get_layout_settings_version();

        let categories_start = Instant::now();
        self.categories = if !self.channel.is_empty() {
            backend::get_categorized_emotes(&self.channel)
        } else {
            Arc::new(vec![])
        };
        record_picker_stage(
            "picker.load.categories_lookup",
            categories_start,
            self.categories.len(),
        );

        self.last_channel_emote_version = channel_version;
        self.last_global_emote_version = global_version;
        self.last_user_emote_version = user_version;
        self.last_layout_settings_version = layout_version;
        let cache_start = Instant::now();
        self.refresh_cached_emotes();
        self.categories_signature = Self::compute_categories_signature(self.categories.as_ref());
        record_picker_stage(
            "picker.load.refresh_cached",
            cache_start,
            self.all_emotes_cache.len(),
        );
        let grid_start = Instant::now();
        self.rebuild_grid();
        record_picker_stage(
            "picker.load.rebuild_grid",
            grid_start,
            self.flat_emotes.len(),
        );
        record_picker_stage("picker.load.total", total_start, self.flat_emotes.len());
    }

    /// Update channel and reload categories
    pub fn set_channel(&mut self, channel: String) {
        if self.channel != channel {
            let total_start = Instant::now();
            self.channel = channel.clone();
            let categories_start = Instant::now();
            if !self.channel.is_empty() {
                self.categories = backend::get_categorized_emotes(&self.channel);
                self.last_channel_emote_version =
                    crate::backend::emotes::get_emote_cache_version(&self.channel);
            } else {
                self.categories = Arc::new(vec![]);
                self.last_channel_emote_version = 0;
            }
            self.last_global_emote_version = crate::backend::emotes::get_global_emote_version();
            self.last_user_emote_version = crate::state::VersionRegistry::user_emote_version();
            self.last_layout_settings_version =
                crate::backend::settings::get_layout_settings_version();
            record_picker_stage(
                "picker.channel.categories_lookup",
                categories_start,
                self.categories.len(),
            );
            let cache_start = Instant::now();
            self.refresh_cached_emotes();
            self.categories_signature =
                Self::compute_categories_signature(self.categories.as_ref());
            record_picker_stage(
                "picker.channel.refresh_cached",
                cache_start,
                self.all_emotes_cache.len(),
            );
            let grid_start = Instant::now();
            self.rebuild_grid();
            record_picker_stage(
                "picker.channel.rebuild_grid",
                grid_start,
                self.flat_emotes.len(),
            );
            record_picker_stage("picker.channel.total", total_start, self.flat_emotes.len());
        }
    }

    /// Set search query and rebuild grid
    pub fn set_search(&mut self, query: String) {
        if self.search_query == query {
            return;
        }
        self.search_query = query;
        self.rebuild_grid();
    }

    /// Select a tab and rebuild grid
    pub fn select_tab(&mut self, tab: EmoteTab) {
        if self.active_tab == tab {
            return;
        }
        self.active_tab = tab;
        self.rebuild_grid();
    }

    /// Get the current loading state
    pub fn get_loading_state(&self) -> LoadingState {
        if !self.categories.is_empty()
            || (self.active_tab == EmoteTab::Emoji && !self.flat_emotes.is_empty())
        {
            return LoadingState::Loaded;
        }

        if self.channel.is_empty() {
            return LoadingState::Empty;
        }

        let channel_version = crate::backend::emotes::get_emote_cache_version(&self.channel);
        let global_version = crate::backend::emotes::get_global_emote_version();

        if channel_version == 0 && global_version == 0 {
            LoadingState::Loading
        } else {
            LoadingState::Empty
        }
    }

    pub fn empty_state_message(&self) -> String {
        if !self.search_query.is_empty() {
            return "No emotes match your search".to_string();
        }

        match self.active_tab {
            EmoteTab::Favorites => "No favorites yet. Right-click to add!".to_string(),
            EmoteTab::Recent => "No recent emotes yet".to_string(),
            EmoteTab::Subs => "No subscription emotes".to_string(),
            EmoteTab::Emoji => "No emoji available".to_string(),
            EmoteTab::Channel => {
                let mut disabled = Vec::new();
                if !crate::backend::settings::get_enable_seventv_emotes() {
                    disabled.push("7TV");
                }
                if !crate::backend::settings::get_enable_bttv_emotes() {
                    disabled.push("BTTV");
                }
                if !crate::backend::settings::get_enable_ffz_emotes() {
                    disabled.push("FFZ");
                }

                if disabled.len() == 3 {
                    "7TV, BTTV, and FFZ channel emotes are disabled in settings".to_string()
                } else {
                    "No channel emotes available for this channel".to_string()
                }
            }
            EmoteTab::Global => "No global emotes available".to_string(),
        }
    }

    /// Check if emotes have been updated and refresh if needed
    /// Returns true if the picker was refreshed
    pub fn check_for_emote_updates(&mut self) -> bool {
        let total_start = Instant::now();
        if self.channel.is_empty() {
            return false;
        }

        let channel_version = crate::backend::emotes::get_emote_cache_version(&self.channel);
        let global_version = crate::backend::emotes::get_global_emote_version();
        let user_version = crate::state::VersionRegistry::user_emote_version();
        let layout_version = crate::backend::settings::get_layout_settings_version();
        let emoji_style = crate::backend::settings::get_emoji_style();
        let version_changed = channel_version != self.last_channel_emote_version
            || global_version != self.last_global_emote_version
            || user_version != self.last_user_emote_version
            || layout_version != self.last_layout_settings_version;
        let emoji_style_changed = emoji_style != self.catalog_emoji_style;

        let categories_start = Instant::now();
        let latest_categories = backend::get_categorized_emotes(&self.channel);
        record_picker_stage(
            "picker.check_updates.categories_lookup",
            categories_start,
            latest_categories.len(),
        );

        if version_changed {
            self.last_channel_emote_version = channel_version;
            self.last_global_emote_version = global_version;
            self.last_user_emote_version = user_version;
            self.last_layout_settings_version = layout_version;
        }

        let categories_changed = (!latest_categories.is_empty() || !self.categories.is_empty())
            && !Arc::ptr_eq(&latest_categories, &self.categories);

        if categories_changed {
            let signature_start = Instant::now();
            let latest_signature = Self::compute_categories_signature(latest_categories.as_ref());
            record_picker_stage(
                "picker.check_updates.categories_signature",
                signature_start,
                latest_categories.len(),
            );
            if latest_signature == self.categories_signature && !emoji_style_changed {
                self.categories = latest_categories;
                record_picker_stage("picker.check_updates.categories_unchanged", total_start, 0);
                return false;
            }

            let refresh_versions_start = Instant::now();
            self.categories = latest_categories;
            self.refresh_cached_emotes();
            self.categories_signature = latest_signature;
            self.rebuild_grid();
            record_picker_stage(
                "picker.check_updates.refresh_categories",
                refresh_versions_start,
                self.flat_emotes.len(),
            );
            record_picker_stage(
                "picker.check_updates.total",
                total_start,
                self.flat_emotes.len(),
            );
            return true;
        }

        if emoji_style_changed {
            let refresh_versions_start = Instant::now();
            self.refresh_cached_emotes();
            self.rebuild_grid();
            record_picker_stage(
                "picker.check_updates.refresh_emoji_style",
                refresh_versions_start,
                self.flat_emotes.len(),
            );
            record_picker_stage(
                "picker.check_updates.total",
                total_start,
                self.flat_emotes.len(),
            );
            return true;
        }

        if version_changed {
            record_picker_stage("picker.check_updates.version_only", total_start, 0);
            return false;
        }

        record_picker_stage("picker.check_updates.total", total_start, 0);
        false
    }

    /// Adopt a catalog handed over by another picker surface.
    ///
    /// The category `Arc` identity is the freshness proof: the bridge returns a
    /// stable `Arc` for one emote-version set, so a pointer match means the
    /// handed-over catalog was flattened from exactly this data.
    fn adopt_catalog_handoff(&mut self, emoji_style: crate::backend::settings::EmojiStyle) -> bool {
        let Some(handoff) = catalog_handoff_slot().take() else {
            return false;
        };
        if handoff.emoji_style != emoji_style
            || handoff.channel != echo_core::channel_identity::normalize_channel_key(&self.channel)
            || !Arc::ptr_eq(&handoff.categories, &self.categories)
        {
            return false;
        }

        self.all_emotes_cache = handoff.all_emotes;
        self.search_index = handoff.search_index;
        true
    }

    /// Offer this surface's catalog to the next picker surface that opens.
    pub(crate) fn publish_catalog_handoff(&self) {
        if self.all_emotes_cache.is_empty() {
            return;
        }
        *catalog_handoff_slot() = Some(PickerCatalogHandoff {
            channel: echo_core::channel_identity::normalize_channel_key(&self.channel),
            categories: Arc::clone(&self.categories),
            emoji_style: self.catalog_emoji_style,
            all_emotes: Arc::clone(&self.all_emotes_cache),
            search_index: Arc::clone(&self.search_index),
        });
    }

    /// Refresh flattened emote/search caches after category updates.
    fn refresh_cached_emotes(&mut self) {
        let started_at = Instant::now();
        let emoji_style = crate::backend::settings::get_emoji_style();
        if self.adopt_catalog_handoff(emoji_style) {
            self.catalog_emoji_style = emoji_style;
            record_picker_stage(
                "picker.refresh_cached_emotes.handoff",
                started_at,
                self.all_emotes_cache.len(),
            );
            return;
        }

        let picker_emoji_emotes = build_picker_emoji_emotes(emoji_style);
        let total = self
            .categories
            .iter()
            .map(|category| category.emotes.len())
            .sum::<usize>()
            + picker_emoji_emotes.len();

        let mut all_emotes_cache: Vec<Arc<Emote>> = Vec::with_capacity(total);

        for category in self.categories.iter() {
            for emote in category.emotes.iter() {
                all_emotes_cache.push(Arc::clone(emote));
            }
        }

        for emote_arc in picker_emoji_emotes {
            all_emotes_cache.push(emote_arc);
        }
        self.search_index = Arc::new(EmoteSearchIndex::new(
            &self.channel,
            all_emotes_cache.iter().cloned(),
        ));
        self.all_emotes_cache = Arc::new(all_emotes_cache);
        self.catalog_emoji_style = emoji_style;
        record_picker_stage(
            "picker.refresh_cached_emotes",
            started_at,
            self.all_emotes_cache.len(),
        );
    }

    fn compute_categories_signature(categories: &[EmoteCategory]) -> u64 {
        fn provider_tag(provider: &EmoteProvider) -> u8 {
            match provider {
                EmoteProvider::Twitch => 0,
                EmoteProvider::SevenTV => 1,
                EmoteProvider::BTTV => 2,
                EmoteProvider::FFZ => 3,
                EmoteProvider::Emoji => 4,
            }
        }

        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        categories.len().hash(&mut hasher);
        for category in categories {
            category.emotes.len().hash(&mut hasher);
            for emote in &category.emotes {
                emote.id.hash(&mut hasher);
                provider_tag(&emote.provider).hash(&mut hasher);
                emote.name.hash(&mut hasher);
                emote.original_name.hash(&mut hasher);
                emote.channel.hash(&mut hasher);
                emote.is_animated.hash(&mut hasher);
                emote.width.hash(&mut hasher);
                emote.height.hash(&mut hasher);
                emote.url.hash(&mut hasher);
                emote.url_1x.hash(&mut hasher);
                emote.url_2x.hash(&mut hasher);
                emote.url_4x.hash(&mut hasher);
            }
        }
        hasher.finish()
    }

    /// Rebuild grid items based on current tab and search
    pub fn rebuild_grid(&mut self) {
        let started_at = Instant::now();
        let all = Arc::clone(&self.all_emotes_cache);
        self.grid_items.clear();
        self.flat_emotes.clear();

        if !self.search_query.is_empty() {
            let preferences = RankingPreferences::snapshot();
            self.cached_favorites = preferences.favorites.clone();
            let matching: Vec<Arc<Emote>> = self
                .search_index
                .rank_with_preferences(&self.search_query, &preferences, None, false)
                .into_iter()
                .map(|ranked| ranked.emote)
                .collect();

            if !matching.is_empty() {
                let count = matching.len();
                self.flat_emotes = matching;
                self.grid_items.push(GridItem::Emotes { range: 0..count });
            }
            record_picker_stage(
                "picker.rebuild_grid.search",
                started_at,
                self.flat_emotes.len(),
            );
            return;
        }

        self.cached_favorites = get_preferences().favorites_set().clone();

        match self.active_tab {
            EmoteTab::Favorites => self.build_favorites_grid(&all),
            EmoteTab::Recent => self.build_recent_grid(&all),
            EmoteTab::Subs => {
                let usage_counts = self.build_usage_counts(&all);
                self.build_subs_grid(&all, &usage_counts);
            }
            EmoteTab::Channel => {
                let usage_counts = self.build_usage_counts(&all);
                self.build_channel_grid(&all, &usage_counts);
            }
            EmoteTab::Global => {
                let usage_counts = self.build_usage_counts(&all);
                self.build_global_grid(&all, &usage_counts);
            }
            EmoteTab::Emoji => self.build_emoji_grid(&all),
        }
        let stage = match self.active_tab {
            EmoteTab::Favorites => "picker.rebuild_grid.favorites",
            EmoteTab::Recent => "picker.rebuild_grid.recent",
            EmoteTab::Subs => "picker.rebuild_grid.subs",
            EmoteTab::Channel => "picker.rebuild_grid.channel",
            EmoteTab::Global => "picker.rebuild_grid.global",
            EmoteTab::Emoji => "picker.rebuild_grid.emoji",
        };
        record_picker_stage(stage, started_at, self.flat_emotes.len());
    }

    fn build_usage_counts(&self, all: &[Arc<Emote>]) -> HashMap<String, u32> {
        let prefs = get_preferences();
        let mut usage_counts: HashMap<String, u32> = HashMap::with_capacity(all.len());
        for emote in all {
            let key = emote_preference_key(emote);
            if usage_counts.contains_key(&key) {
                continue;
            }
            let count = prefs.get_usage_count(&emote.id, provider_preference_key(&emote.provider));
            usage_counts.insert(key, count);
        }
        usage_counts
    }

    /// Check if a Twitch channel name is a "special" global-like channel
    /// These are event emotes, promo emotes, or numeric IDs that should be grouped as global
    fn is_global_twitch_channel(channel_name: &str) -> bool {
        if channel_name.chars().all(|c| c.is_ascii_digit()) {
            return true;
        }
        let global_channels = [
            "celebratekpop",
            "hindsight2020",
            "hindsight2020emotes",
            "hyperscape",
            "hyperscapeemotes",
            "streamwithpride",
            "twitch",
            "twitchgaming",
            "twitchprime",
            "twitchcon",
            "twitchdev",
        ];
        global_channels.contains(&channel_name.to_lowercase().as_str())
    }

    /// Check if a Twitch channel name is a Hype Train channel
    /// These get their own "Twitch Hype Train" category in the Global tab
    fn is_hype_train_channel(channel_name: &str) -> bool {
        let hype_train_channels = ["twitchhype", "twitchhypetrain"];
        hype_train_channels.contains(&channel_name.to_lowercase().as_str())
    }

    /// Build grid for Favorites tab
    fn build_favorites_grid(&mut self, all: &[Arc<Emote>]) {
        let prefs = get_preferences();

        let favorites: Vec<Arc<Emote>> = all
            .iter()
            .filter(|e| {
                let provider_str = provider_preference_key(&e.provider);
                prefs.is_favorite(&e.id, provider_str)
            })
            .cloned()
            .collect();

        if !favorites.is_empty() {
            self.add_section(
                "special-favorites",
                "Favorites",
                "♥",
                &favorites,
                true,
                None,
            );
        }
    }

    /// Build grid for Recent tab
    fn build_recent_grid(&mut self, all: &[Arc<Emote>]) {
        let prefs = get_preferences();

        let recent_refs = prefs.get_recent(RECENT_EMOTES_COUNT);
        let recent: Vec<Arc<Emote>> = recent_refs
            .iter()
            .filter_map(|r| {
                all.iter()
                    .find(|e| {
                        e.id.as_ref() == r.id && provider_preference_key(&e.provider) == r.provider
                    })
                    .cloned()
                    .or_else(|| all.iter().find(|e| e.name.as_ref() == r.name).cloned())
            })
            .collect();

        if !recent.is_empty() {
            self.add_section("special-recent", "Recently Used", "🕐", &recent, true, None);
        }
    }

    /// Build grid for Subs tab - all Twitch subscription emotes grouped by channel
    fn build_subs_grid(&mut self, all: &[Arc<Emote>], usage_counts: &HashMap<String, u32>) {
        use std::collections::HashMap;

        let mut sub_emotes: HashMap<String, Vec<Arc<Emote>>> = HashMap::new();

        for emote in all.iter() {
            if emote.provider == EmoteProvider::Twitch
                && let Some(ref ch) = emote.channel
            {
                if ch.starts_with("sub:") {
                    let channel_name = ch.strip_prefix("sub:").unwrap_or(ch);
                    if !Self::is_global_twitch_channel(channel_name)
                        && !Self::is_hype_train_channel(channel_name)
                    {
                        if let Some(existing) = sub_emotes.get_mut(channel_name) {
                            existing.push(Arc::clone(emote));
                        } else {
                            sub_emotes.insert(channel_name.to_string(), vec![Arc::clone(emote)]);
                        }
                    }
                } else if !ch.starts_with("twitch:")
                    && !Self::is_global_twitch_channel(ch)
                    && !Self::is_hype_train_channel(ch)
                {
                    if let Some(existing) = sub_emotes.get_mut(ch.as_ref()) {
                        existing.push(Arc::clone(emote));
                    } else {
                        sub_emotes.insert(ch.to_string(), vec![Arc::clone(emote)]);
                    }
                }
            }
        }

        if sub_emotes.is_empty() {
            return;
        }

        let mut channels: Vec<_> = sub_emotes.into_iter().collect();
        channels.sort_by_cached_key(|(channel, _)| channel.to_lowercase());

        for (channel_name, mut emotes) in channels {
            if !emotes.is_empty() {
                self.sort_by_usage_with_cache(&mut emotes, usage_counts);
                self.add_section(
                    &format!("sub:{}", channel_name),
                    &channel_name,
                    "Tw",
                    &emotes,
                    false,
                    Some(EmoteProvider::Twitch),
                );
            }
        }
    }

    /// Build grid for Channel tab - 7TV/BTTV/FFZ emotes for current channel
    fn build_channel_grid(&mut self, all: &[Arc<Emote>], usage_counts: &HashMap<String, u32>) {
        let channel_lower = self.channel.to_lowercase();

        let providers = [
            (EmoteProvider::SevenTV, "7TV", "7"),
            (EmoteProvider::BTTV, "BetterTTV", "BT"),
            (EmoteProvider::FFZ, "FrankerFaceZ", "FF"),
        ];

        for (provider, display_name, icon) in providers {
            let channel_emotes: Vec<Arc<Emote>> = all
                .iter()
                .filter(|e| {
                    if e.provider != provider {
                        return false;
                    }
                    if let Some(ref ch) = e.channel {
                        ch.to_lowercase() == channel_lower && ch.as_ref() != "personal:7tv"
                    } else {
                        false
                    }
                })
                .cloned()
                .collect();

            if !channel_emotes.is_empty() {
                let mut sorted = channel_emotes;
                self.sort_by_usage_with_cache(&mut sorted, usage_counts);
                self.add_section(
                    &format!("{:?}-channel", provider),
                    display_name,
                    icon,
                    &sorted,
                    false,
                    Some(provider),
                );
            }
        }

        let personal_7tv: Vec<Arc<Emote>> = all
            .iter()
            .filter(|e| {
                e.provider == EmoteProvider::SevenTV && e.channel.as_deref() == Some("personal:7tv")
            })
            .cloned()
            .collect();

        if !personal_7tv.is_empty() {
            let mut sorted = personal_7tv;
            self.sort_by_usage_with_cache(&mut sorted, usage_counts);
            self.add_section(
                "personal:7tv",
                "7TV Personal",
                "7",
                &sorted,
                false,
                Some(EmoteProvider::SevenTV),
            );
        }
    }

    /// Build grid for Global tab - global emotes from all providers + Twitch special categories
    fn build_global_grid(&mut self, all: &[Arc<Emote>], usage_counts: &HashMap<String, u32>) {
        use std::collections::HashMap;

        let mut twitch_categories: HashMap<&str, Vec<Arc<Emote>>> = HashMap::new();
        let mut global_twitch_extras: Vec<Arc<Emote>> = Vec::new();

        for emote in all.iter() {
            if emote.provider == EmoteProvider::Twitch
                && let Some(ref ch) = emote.channel
            {
                if let Some(category) = ch.strip_prefix("twitch:") {
                    twitch_categories
                        .entry(category)
                        .or_default()
                        .push(Arc::clone(emote));
                } else if ch.starts_with("sub:") {
                    let channel_name = ch.strip_prefix("sub:").unwrap_or(ch);
                    if Self::is_global_twitch_channel(channel_name)
                        || Self::is_hype_train_channel(channel_name)
                    {
                        global_twitch_extras.push(Arc::clone(emote));
                    }
                }
            }
        }

        let providers = [
            (EmoteProvider::Twitch, "Twitch", "Tw"),
            (EmoteProvider::SevenTV, "7TV", "7"),
            (EmoteProvider::BTTV, "BetterTTV", "BT"),
            (EmoteProvider::FFZ, "FrankerFaceZ", "FF"),
        ];

        for (provider, display_name, icon) in providers {
            let mut global_emotes: Vec<Arc<Emote>> = all
                .iter()
                .filter(|e| e.provider == provider && e.channel.is_none())
                .cloned()
                .collect();

            if provider == EmoteProvider::Twitch {
                global_emotes.extend(global_twitch_extras.iter().cloned());
            }

            if !global_emotes.is_empty() {
                let mut sorted = global_emotes;
                self.sort_by_usage_with_cache(&mut sorted, usage_counts);
                self.add_section(
                    &format!("{:?}-global", provider),
                    display_name,
                    icon,
                    &sorted,
                    false,
                    Some(provider),
                );
            }
        }

        let category_info = [
            ("prime", "Twitch Prime", "👑"),
            ("turbo", "Twitch Turbo", "⚡"),
            ("hypetrain", "Hype Train", "🚂"),
            ("limited", "Limited Time", "⏰"),
            ("rewards", "Channel Rewards", "🎁"),
        ];

        for (category, display_name, icon) in category_info {
            if let Some(emotes) = twitch_categories.get(category)
                && !emotes.is_empty()
            {
                let mut sorted = emotes.clone();
                self.sort_by_usage_with_cache(&mut sorted, usage_counts);
                self.add_section(
                    &format!("twitch-{}", category),
                    display_name,
                    icon,
                    &sorted,
                    false,
                    Some(EmoteProvider::Twitch),
                );
            }
        }
    }

    /// Build grid for Emoji tab - emoji from various styles
    fn build_emoji_grid(&mut self, all: &[Arc<Emote>]) {
        for group in Group::iter() {
            let group_key = format!("emoji:{}", emoji_group_id(group));
            let emotes: Vec<Arc<Emote>> = all
                .iter()
                .filter(|emote| {
                    emote.provider == EmoteProvider::Emoji
                        && emote.channel.as_deref() == Some(group_key.as_str())
                })
                .cloned()
                .collect();

            if !emotes.is_empty() {
                self.add_section(
                    &group_key,
                    emoji_group_label(group),
                    emoji_group_icon(group),
                    &emotes,
                    false,
                    Some(EmoteProvider::Emoji),
                );
            }
        }
    }

    /// Sort emotes by usage frequency (most used first)
    fn sort_by_usage_with_cache(
        &self,
        emotes: &mut [Arc<Emote>],
        usage_counts: &HashMap<String, u32>,
    ) {
        emotes.sort_by(|a, b| {
            let key_a = emote_preference_key(a);
            let key_b = emote_preference_key(b);
            let freq_a = usage_counts.get(&key_a).copied().unwrap_or(0);
            let freq_b = usage_counts.get(&key_b).copied().unwrap_or(0);
            freq_b.cmp(&freq_a).then_with(|| a.name.cmp(&b.name))
        });
    }

    /// Add a collapsible section to the grid
    fn add_section(
        &mut self,
        id: &str,
        name: &str,
        icon: &str,
        emotes: &[Arc<Emote>],
        is_special: bool,
        provider: Option<EmoteProvider>,
    ) {
        let is_collapsed = get_preferences().is_category_collapsed(id);

        let emote_count = emotes.len();
        self.grid_items.push(GridItem::Header(CategoryHeader {
            id: id.to_string(),
            name: name.to_string(),
            icon: icon.to_string(),
            count_str: SharedString::from(emote_count.to_string()),
            is_collapsed,
            is_special,
            provider,
        }));

        if !is_collapsed {
            let start = self.flat_emotes.len();
            self.flat_emotes.extend(emotes.iter().map(Arc::clone));
            self.grid_items.push(GridItem::Emotes {
                range: start..self.flat_emotes.len(),
            });
        }
    }

    /// Toggle category collapsed state
    pub fn toggle_category(&mut self, category_id: &str) {
        if let Some(mut prefs) = get_preferences_mut() {
            prefs.toggle_category_collapsed(category_id);
            prefs.try_save();
        }
        self.rebuild_grid();
    }

    /// Record emote usage and save preferences
    pub fn record_usage(&mut self, emote: &Emote) {
        record_emote_usage(emote);
    }

    /// Toggle favorite status for an emote
    pub fn toggle_favorite(&mut self, emote: &Emote) {
        if let Some(mut prefs) = get_preferences_mut() {
            let provider_str = provider_preference_key(&emote.provider);
            prefs.toggle_favorite(&emote.id, provider_str, &emote.name);
            prefs.try_save();
        }
        self.rebuild_grid();
    }

    /// Check if an emote is a favorite (uses cached set - O(1) without lock)
    pub fn is_favorite(&self, emote: &Emote) -> bool {
        let key = emote_preference_key(emote);
        self.cached_favorites.contains(&key)
    }
}

/// Horizontal gap between emote cells within a packed row.
const EMOTE_ROW_GAP: f32 = 4.0;

/// Minimum width a packed row is allowed to assume.
const PACK_MIN_ROW_WIDTH: f32 = 120.0;

#[derive(Clone, Debug)]
struct PackedNavigationRow {
    virtual_row_index: usize,
    base_index: usize,
    cell_centers: Vec<f32>,
}

/// Navigation geometry captured from the same variable-width packing pass used
/// to render picker rows.
#[derive(Clone, Debug, Default)]
pub struct PackedGridNavigation {
    rows: Vec<PackedNavigationRow>,
}

impl PackedGridNavigation {
    fn push_row(
        &mut self,
        virtual_row_index: usize,
        base_index: usize,
        emotes: &[(Arc<Emote>, bool)],
    ) {
        let mut x = 0.0f32;
        let mut cell_centers = Vec::with_capacity(emotes.len());
        for (index, (emote, _)) in emotes.iter().enumerate() {
            let (cell_width, _, _, _) = calculate_emote_dimensions(emote);
            if index > 0 {
                x += EMOTE_ROW_GAP;
            }
            cell_centers.push(x + cell_width / 2.0);
            x += cell_width;
        }
        self.rows.push(PackedNavigationRow {
            virtual_row_index,
            base_index,
            cell_centers,
        });
    }

    fn row_position_for_index(&self, selected_index: usize) -> Option<usize> {
        self.rows.iter().position(|row| {
            selected_index >= row.base_index
                && selected_index < row.base_index + row.cell_centers.len()
        })
    }

    pub(crate) fn virtual_row_for_index(&self, selected_index: usize) -> Option<usize> {
        self.row_position_for_index(selected_index)
            .map(|position| self.rows[position].virtual_row_index)
    }

    pub(crate) fn row_edge(&self, selected_index: usize, end: bool) -> Option<usize> {
        let row = self
            .rows
            .get(self.row_position_for_index(selected_index)?)?;
        if end {
            row.base_index
                .checked_add(row.cell_centers.len().checked_sub(1)?)
        } else {
            Some(row.base_index)
        }
    }

    pub(crate) fn vertical_neighbor(
        &self,
        selected_index: usize,
        direction: isize,
    ) -> Option<usize> {
        let row_position = self.row_position_for_index(selected_index)?;
        let target_position = row_position.checked_add_signed(direction)?;
        let current_row = self.rows.get(row_position)?;
        let target_row = self.rows.get(target_position)?;
        Self::closest_to_center(current_row, target_row, selected_index)
    }

    pub(crate) fn page_neighbor(
        &self,
        selected_index: usize,
        direction: isize,
        visible_rows: usize,
    ) -> Option<usize> {
        let row_position = self.row_position_for_index(selected_index)?;
        let current_row = self.rows.get(row_position)?;
        let distance = visible_rows.max(1);
        let target_position = if direction < 0 {
            let target_virtual_row = current_row.virtual_row_index.saturating_sub(distance);
            self.rows
                .partition_point(|row| row.virtual_row_index <= target_virtual_row)
                .saturating_sub(1)
        } else if direction > 0 {
            let target_virtual_row = current_row.virtual_row_index.saturating_add(distance);
            self.rows
                .partition_point(|row| row.virtual_row_index < target_virtual_row)
                .min(self.rows.len().checked_sub(1)?)
        } else {
            return Some(selected_index);
        };
        let target_row = self.rows.get(target_position)?;
        Self::closest_to_center(current_row, target_row, selected_index)
    }

    fn closest_to_center(
        current_row: &PackedNavigationRow,
        target_row: &PackedNavigationRow,
        selected_index: usize,
    ) -> Option<usize> {
        let current_column = selected_index.checked_sub(current_row.base_index)?;
        let current_center = *current_row.cell_centers.get(current_column)?;
        let target_column = target_row
            .cell_centers
            .iter()
            .enumerate()
            .min_by(|(_, left), (_, right)| {
                (*left - current_center)
                    .abs()
                    .total_cmp(&(*right - current_center).abs())
            })
            .map(|(index, _)| index)?;
        Some(target_row.base_index + target_column)
    }
}

/// Pack `grid_items` into virtualized rows that fit `row_width_hint`.
///
/// This is the shared grid-packing logic used by both the inline emote picker
/// and the popout window picker. It is generic over the caller's virtual-row
/// type `R`: each caller supplies constructors for the three row variants so
/// it keeps its own local enum and rendering code unchanged.
///
/// Returns `(rows, shown, total_available, navigation)` where `shown` is the number of
/// emotes actually placed into rows and `total_available` is the total number
/// of emotes in `flat_emotes`. Callers are responsible for any post-pack
/// behavior (metrics labeling, perf logging, selection clamping, assignment).
#[allow(clippy::too_many_arguments)]
pub fn pack_virtual_rows<R>(
    grid_items: &[GridItem],
    flat_emotes: &[Arc<Emote>],
    visible_emote_limit: usize,
    row_width_hint: f32,
    is_favorite: impl Fn(&Emote) -> bool,
    make_header: impl Fn(CategoryHeader) -> R,
    make_emote_row: impl Fn(Vec<(Arc<Emote>, bool)>, usize) -> R,
    make_load_more: impl Fn(usize, usize) -> R,
) -> (Vec<R>, usize, usize, PackedGridNavigation) {
    let max_row_width = row_width_hint.max(PACK_MIN_ROW_WIDTH);
    let mut rows = Vec::new();
    let mut emote_index = 0usize;
    let total_available = flat_emotes.len();
    let max_visible = visible_emote_limit.min(total_available);
    let mut remaining_visible = max_visible;
    let mut shown = 0usize;
    let mut pending_header: Option<CategoryHeader> = None;
    let mut navigation = PackedGridNavigation::default();

    for item in grid_items.iter() {
        match item {
            GridItem::Header(header) => {
                if remaining_visible == 0 && !header.is_collapsed {
                    break;
                }
                if header.is_collapsed {
                    rows.push(make_header(header.clone()));
                } else {
                    pending_header = Some(header.clone());
                }
            }
            GridItem::Emotes { range } => {
                if remaining_visible == 0 {
                    break;
                }
                let Some(emotes) = flat_emotes.get(range.clone()) else {
                    continue;
                };
                let take_count = emotes.len().min(remaining_visible);
                if take_count == 0 {
                    continue;
                }
                if let Some(header) = pending_header.take() {
                    rows.push(make_header(header));
                }

                let mut row_emotes: Vec<(Arc<Emote>, bool)> = Vec::with_capacity(12);
                let mut row_width = 0.0f32;
                let mut row_base_index = emote_index;

                for emote in emotes.iter().take(take_count) {
                    let (cell_width, _, _, _) = calculate_emote_dimensions(emote);
                    let additional_width = if row_emotes.is_empty() {
                        cell_width
                    } else {
                        EMOTE_ROW_GAP + cell_width
                    };

                    let row_full_by_width =
                        !row_emotes.is_empty() && (row_width + additional_width) > max_row_width;
                    if row_full_by_width {
                        navigation.push_row(rows.len(), row_base_index, &row_emotes);
                        rows.push(make_emote_row(row_emotes, row_base_index));
                        row_emotes = Vec::with_capacity(12);
                        row_width = 0.0;
                        row_base_index = emote_index;
                    }

                    let width_to_add = if row_emotes.is_empty() {
                        cell_width
                    } else {
                        EMOTE_ROW_GAP + cell_width
                    };
                    row_width += width_to_add;
                    row_emotes.push((Arc::clone(emote), is_favorite(emote)));
                    emote_index += 1;
                    shown += 1;
                    remaining_visible = remaining_visible.saturating_sub(1);
                }

                if !row_emotes.is_empty() {
                    navigation.push_row(rows.len(), row_base_index, &row_emotes);
                    rows.push(make_emote_row(row_emotes, row_base_index));
                }
                if take_count < emotes.len() {
                    break;
                }
            }
        }
    }
    if shown < total_available {
        rows.push(make_load_more(shown, total_available));
    }

    (rows, shown, total_available, navigation)
}

/// Get provider color (delegates to unified utils function)
pub fn provider_color(provider: Option<EmoteProvider>) -> Hsla {
    match provider {
        Some(p) => emote_provider_color(provider_display_name(&p)),
        None => colors::text_muted(),
    }
}

/// Get provider display name
pub fn provider_display_name(provider: &EmoteProvider) -> &'static str {
    match provider {
        EmoteProvider::Twitch => "Twitch",
        EmoteProvider::SevenTV => "7TV",
        EmoteProvider::BTTV => "BetterTTV",
        EmoteProvider::FFZ => "FrankerFaceZ",
        EmoteProvider::Emoji => "Emoji",
    }
}
