//! Autocomplete overlay for Echo GPUI
//!
//! Provides autocomplete for:
//! - Emotes (triggered by : or Tab)
//! - Mentions (triggered by @ or Tab)
//! - Commands (triggered by /)
//!
//! Features fuzzy search with word boundary bonuses, compact emote previews,
//! and quiet provider/scope labels.

use super::emote_picker_core::{
    EmoteScope, EmoteSearchIndex, RankedEmote, build_picker_emoji_emotes,
    emote_display_url_at_height,
};
use super::traits::Openable;
use crate::backend::commands::{
    COMMAND_AUTOCOMPLETE_SPECS, CommandArgumentCompletion, CommandAutocompleteSpec,
    CommandValueSpec,
};
use crate::backend::{self, EmoteProvider};
use crate::cache::disk_layer::MediaSourceId;
use crate::cache::loader as image_cache;
use crate::components::ui::ButtonExt;
use crate::state::SplitId;
use crate::theme::tokens::{FontSize, Radii, RadiiPixels, Spacing};
use crate::theme::{colors, tokens::Sizes};
use crate::utils::fuzzy_score;
use crate::utils::parse_hex_color_opt;
use echo_autocomplete_core::CommandArgumentContext;
pub use echo_autocomplete_core::{AutocompleteType, CompletionContext};
use gpui::prelude::FluentBuilder;
use gpui::*;
use std::collections::HashSet;
use std::ops::Range;
use std::sync::Arc;

/// Maximum number of suggestions to show
const MAX_SUGGESTIONS: usize = 12;

/// Popup dimensions. The candidate list remains larger for keyboard cycling,
/// while the surface exposes six compact rows and scrolls the rest.
const MAX_VISIBLE_SUGGESTIONS: usize = 6;
const POPUP_WIDTH: f32 = 360.0;
const POPUP_MAX_HEIGHT: f32 = ITEM_HEIGHT * MAX_VISIBLE_SUGGESTIONS as f32;
const ITEM_HEIGHT: f32 = 34.0;
const EMOTE_SIZE: f32 = 26.0;
const ICON_SIZE: f32 = Sizes::ICON_LG;
const COMMAND_CHANNEL_QUERY_PREFIX: &str = "channel:";
const COMMAND_MESSAGE_QUERY_PREFIX: &str = "message:";
const COMMAND_PREDICTION_OUTCOME_QUERY_PREFIX: &str = "prediction-outcome:";
const COMMAND_USER_QUERY_PREFIX: &str = "user:";
const COMMAND_VALUE_QUERY_PREFIX: &str = "value:";

/// Emote type for display
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EmoteType {
    Channel,
    Global,
    Personal,
    Sub,
}

impl EmoteType {
    pub fn label(&self) -> &'static str {
        match self {
            EmoteType::Channel => "Channel",
            EmoteType::Global => "Global",
            EmoteType::Personal => "Personal",
            EmoteType::Sub => "Sub",
        }
    }
}

/// Autocomplete suggestion with rich metadata
#[derive(Clone, Debug)]
pub struct AutocompleteSuggestion {
    pub text: String,
    pub image_url: Option<String>,
    pub subtitle: Option<String>,
    pub color: Option<String>,
    /// Provider for emotes (7TV, BTTV, FFZ, Twitch)
    pub provider: Option<String>,
    /// Type for emotes (Channel, Global, Sub, Personal)
    pub emote_type: Option<EmoteType>,
    /// Preserve the provider's explicit animation declaration through the
    /// ranking/render boundary. URL suffixes are not a reliable signal.
    pub is_animated: bool,
    /// Stable media-source identity for cache de-duplication and transient
    /// autocomplete preview loading.
    pub media_identity: Option<MediaSourceId>,
    /// Fuzzy match score for sorting
    pub score: f64,
}

/// Owned snapshot of a suggestion and the input context it belongs to.
#[derive(Clone, Debug)]
pub struct AutocompleteSelection {
    pub split_id: SplitId,
    pub channel: String,
    pub context: CompletionContext,
    pub text: String,
    pub is_emote: bool,
    /// Picker URL of the chosen emote, to tell same-named emotes apart.
    pub emote_image_url: Option<String>,
}

/// One frozen candidate used by inline Tab completion.
///
/// The visual `:`/`@` suggestion list is intentionally separate. Inline Tab
/// completion keeps its original candidate list while the user cycles, so a
/// replacement cannot rerank itself after the first key press.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InlineTabCompletionCandidate {
    pub text: String,
    pub is_emote: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InlineTabCompletionEdit {
    pub replacement_range: Range<usize>,
    pub expected_fragment: String,
    pub text: String,
    pub is_emote: bool,
    pub selected_index: usize,
    pub candidate_count: usize,
}

/// Frozen inline completion state shared by the main composer and thread
/// reply input. First Tab always selects row zero; subsequent Tab/Shift+Tab
/// presses cycle with wrapping.
#[derive(Clone, Debug)]
pub struct InlineTabCompletionSession {
    candidates: Vec<InlineTabCompletionCandidate>,
    selected_index: usize,
    has_applied: bool,
    replacement_range: Range<usize>,
    expected_fragment: String,
    expected_cursor: usize,
}

impl InlineTabCompletionSession {
    pub fn new(
        context: CompletionContext,
        candidates: Vec<InlineTabCompletionCandidate>,
    ) -> Option<Self> {
        if context.ac_type != AutocompleteType::Tab || candidates.is_empty() {
            return None;
        }
        Some(Self {
            candidates,
            selected_index: 0,
            has_applied: false,
            replacement_range: context.replacement_range,
            expected_fragment: context.expected_fragment,
            expected_cursor: 0,
        })
    }

    pub fn matches(&self, text: &str, cursor: usize) -> bool {
        if !self.has_applied || cursor != self.expected_cursor {
            return false;
        }
        text.get(self.replacement_range.clone())
            .is_some_and(|fragment| fragment == self.expected_fragment)
    }

    pub fn next_edit(&mut self, reverse: bool) -> InlineTabCompletionEdit {
        if self.has_applied || reverse {
            if reverse {
                self.selected_index = self
                    .selected_index
                    .checked_sub(1)
                    .unwrap_or(self.candidates.len() - 1);
            } else {
                self.selected_index = (self.selected_index + 1) % self.candidates.len();
            }
        }
        let candidate = &self.candidates[self.selected_index];
        InlineTabCompletionEdit {
            replacement_range: self.replacement_range.clone(),
            expected_fragment: self.expected_fragment.clone(),
            text: candidate.text.clone(),
            is_emote: candidate.is_emote,
            selected_index: self.selected_index,
            candidate_count: self.candidates.len(),
        }
    }

    pub fn commit_applied(&mut self, edit: &InlineTabCompletionEdit, cursor: usize) {
        self.has_applied = true;
        self.replacement_range =
            edit.replacement_range.start..edit.replacement_range.start + edit.text.len();
        self.expected_fragment.clone_from(&edit.text);
        self.expected_cursor = cursor;
    }
}

/// Autocomplete state
#[derive(Clone, Debug)]
pub struct AutocompleteState {
    pub is_open: bool,
    pub x: f32,
    pub y: f32,
    pub channel: String,
    /// Stable split identifier (survives layout changes).
    pub split_id: SplitId,
    pub query: String,
    pub ac_type: AutocompleteType,
    pub context: CompletionContext,
    pub selected_index: usize,
}

impl Default for AutocompleteState {
    fn default() -> Self {
        Self {
            is_open: false,
            x: 0.0,
            y: 0.0,
            channel: String::new(),
            split_id: SplitId::new(),
            query: String::new(),
            ac_type: AutocompleteType::Emote,
            context: CompletionContext::default(),
            selected_index: 0,
        }
    }
}

impl Openable for AutocompleteState {
    fn set_open(&mut self, open: bool) {
        self.is_open = open;
    }
}

impl AutocompleteState {
    /// Open autocomplete for a stable split id.
    pub fn open(
        x: f32,
        y: f32,
        channel: String,
        split_id: SplitId,
        context: CompletionContext,
    ) -> Self {
        let query = context.query.clone();
        let ac_type = context.ac_type;
        Self {
            is_open: true,
            x,
            y,
            channel,
            split_id,
            query,
            ac_type,
            context,
            selected_index: 0,
        }
    }

    /// Close the autocomplete and clear state
    pub fn close(&mut self) {
        Openable::close(self);
        self.query.clear();
        self.context.query.clear();
        self.context.replacement_range = 0..0;
        self.context.expected_fragment.clear();
        self.selected_index = 0;
    }
}

/// Autocomplete component
pub struct Autocomplete {
    pub state: AutocompleteState,
    pub suggestions: Vec<AutocompleteSuggestion>,
    /// The same handle serves standalone and inline completion surfaces. It is
    /// moved only for explicit keyboard selection changes, never while a user
    /// scrolls the popup manually.
    scroll_handle: ScrollHandle,
    suggestion_revision: u64,
    last_emote_generations: Option<(u32, u32, u64, u64)>,
    /// Layout scale the current suggestions' image tiers were chosen for.
    suggestions_scale_bits: u32,
    emote_index_generations: Option<(u32, u32, u64, u64)>,
    emote_index_channel: String,
    emote_search_index: Option<EmoteSearchIndex>,
    emote_index_includes_emoji: bool,
    on_select: Option<Arc<dyn Fn(String) + Send + Sync>>,
}

impl Autocomplete {
    pub fn new(state: AutocompleteState) -> Self {
        let mut instance = Self {
            state,
            suggestions: vec![],
            scroll_handle: ScrollHandle::new(),
            suggestion_revision: 0,
            last_emote_generations: None,
            suggestions_scale_bits: 0,
            emote_index_generations: None,
            emote_index_channel: String::new(),
            emote_search_index: None,
            emote_index_includes_emoji: false,
            on_select: None,
        };
        instance.update_suggestions();
        instance
    }

    pub fn update_state(&mut self, state: AutocompleteState) {
        let needs_update = self.state.query != state.query
            || self.state.ac_type != state.ac_type
            || self.state.channel != state.channel;
        let selection_target_changed =
            self.state.split_id != state.split_id || self.state.context != state.context;
        let selected_index = self.state.selected_index;
        self.state = state;
        if needs_update {
            self.update_suggestions();
            if self.state.selected_index >= self.suggestions.len() {
                self.state.selected_index = 0;
            }
            self.reveal_selected_index();
        } else if selection_target_changed {
            self.advance_suggestion_revision();
        } else {
            self.state.selected_index = selected_index;
        }
    }

    /// Revision of the currently published suggestion list.
    pub fn suggestion_revision(&self) -> u64 {
        self.suggestion_revision
    }

    /// Scroll handle for completion surfaces that render the shared candidate
    /// list as direct children.
    pub fn scroll_handle(&self) -> &ScrollHandle {
        &self.scroll_handle
    }

    /// Select one candidate in response to keyboard navigation and reveal it
    /// with the handle's minimal-scroll strategy. Pointer/wheel interactions
    /// intentionally do not call this, preserving the user's manual position.
    pub fn select_index_from_keyboard(&mut self, index: usize) {
        let Some(last_index) = self.suggestions.len().checked_sub(1) else {
            self.state.selected_index = 0;
            return;
        };
        let index = index.min(last_index);
        if self.state.selected_index == index {
            return;
        }
        self.state.selected_index = index;
        self.prefetch_visible_suggestions(
            index.saturating_sub(MAX_VISIBLE_SUGGESTIONS - 1),
            MAX_VISIBLE_SUGGESTIONS,
        );
        self.reveal_selected_index();
    }

    /// Request GPUI's minimal scroll only after a list/keyboard selection
    /// change. Render and wheel paths leave the handle untouched.
    fn reveal_selected_index(&self) {
        if self.state.selected_index < self.suggestions.len() {
            self.scroll_handle.scroll_to_item(self.state.selected_index);
        }
    }

    /// Return an owned suggestion snapshot only when the rendered revision is current.
    pub fn selection_at(
        &self,
        expected_revision: u64,
        index: usize,
    ) -> Option<AutocompleteSelection> {
        if !self.state.is_open || expected_revision != self.suggestion_revision {
            return None;
        }

        let suggestion = self.suggestions.get(index)?;
        Some(AutocompleteSelection {
            split_id: self.state.split_id,
            channel: self.state.channel.clone(),
            context: self.state.context.clone(),
            text: self.selection_text(suggestion),
            is_emote: suggestion.emote_type.is_some(),
            emote_image_url: suggestion
                .emote_type
                .is_some()
                .then(|| suggestion.image_url.clone())
                .flatten(),
        })
    }

    fn selection_text(&self, suggestion: &AutocompleteSuggestion) -> String {
        if self.state.ac_type != AutocompleteType::Mention {
            return suggestion.text.clone();
        }

        let username = suggestion.text.trim_start_matches('@');
        let comma = self.state.context.replacement_range.start == 0
            && crate::backend::settings::get_mention_users_with_comma();
        if comma {
            format!("@{username},")
        } else {
            format!("@{username}")
        }
    }

    /// Return the uncapped, frozen candidate set used by inline Tab
    /// completion. This deliberately differs from the compact visual popup:
    /// cycling is prefix-only, de-duplicates provider aliases
    /// by insertion text, puts emotes before users, and keeps bare usernames
    /// bare instead of adding `@`.
    pub fn inline_tab_candidates(&self) -> Vec<InlineTabCompletionCandidate> {
        if self.state.ac_type != AutocompleteType::Tab
            || self.state.channel.is_empty()
            || self.state.query.chars().count() < 2
        {
            return Vec::new();
        }

        let query = self.state.query.to_lowercase();
        let mut candidates = Vec::new();
        let mut inserted_text = HashSet::new();

        if let Some(index) = self.emote_search_index.as_ref() {
            for ranked in index.rank_prefix(&self.state.query) {
                debug_assert!(ranked.is_prefix_match);
                let text = ranked.emote.name.to_string();
                if inserted_text.insert(text.clone()) {
                    candidates.push(InlineTabCompletionCandidate {
                        text,
                        is_emote: true,
                    });
                }
            }
        }

        if crate::backend::settings::get_tab_complete_usernames() {
            let mut chatters =
                backend::irc::get_ranked_chatters(&self.state.channel, &query, usize::MAX);
            let broadcaster_login = self
                .state
                .channel
                .trim_start_matches('#')
                .to_ascii_lowercase();
            if !broadcaster_login.is_empty()
                && broadcaster_login.starts_with(&query)
                && !chatters
                    .iter()
                    .any(|(login, _, _)| login.eq_ignore_ascii_case(&broadcaster_login))
            {
                chatters.push((
                    broadcaster_login.clone(),
                    self.state.channel.trim_start_matches('#').to_string(),
                    None,
                ));
            }

            let first_word = self.state.context.replacement_range.start == 0;
            let comma = first_word && crate::backend::settings::get_mention_users_with_comma();
            let mut seen_logins = HashSet::new();
            for (login, display, _) in chatters {
                let login_lower = login.to_lowercase();
                if !seen_logins.insert(login_lower) {
                    continue;
                }
                let mut text = if display.trim().is_empty() {
                    login
                } else {
                    display
                };
                if comma {
                    text.push(',');
                }
                if inserted_text.insert(text.clone()) {
                    candidates.push(InlineTabCompletionCandidate {
                        text,
                        is_emote: false,
                    });
                }
            }
        }

        candidates
    }

    /// Refresh cached emote-backed suggestions without disturbing other autocomplete modes.
    ///
    /// Emote generations can advance while the popup is already open and its query is
    /// unchanged. Callers use the return value to notify GPUI only when an open Emote/Tab
    /// popup actually consumed that publication.
    pub fn refresh_open_emote_suggestions(&mut self) -> bool {
        if !self.state.is_open
            || !matches!(
                self.state.ac_type,
                AutocompleteType::Emote | AutocompleteType::Tab
            )
        {
            return false;
        }

        let generations = self.current_emote_generations();
        if self.last_emote_generations == Some(generations)
            && self.suggestions_scale_bits
                == crate::components::chat::canvas_renderer::layout_scale::current_bits()
        {
            return false;
        }

        let selected_text = self
            .suggestions
            .get(self.state.selected_index)
            .map(|suggestion| suggestion.text.clone());
        self.update_suggestions();
        if let Some(selected_text) = selected_text {
            if let Some(index) = self
                .suggestions
                .iter()
                .position(|suggestion| suggestion.text == selected_text)
            {
                self.state.selected_index = index;
            } else if self.state.selected_index >= self.suggestions.len() {
                self.state.selected_index = self.suggestions.len().saturating_sub(1);
            }
        } else if self.state.selected_index >= self.suggestions.len() {
            self.state.selected_index = 0;
        }
        self.reveal_selected_index();
        true
    }

    fn update_suggestions(&mut self) {
        let emote_backed = matches!(
            self.state.ac_type,
            AutocompleteType::Emote | AutocompleteType::Tab
        );
        let emote_generations = emote_backed.then(|| self.current_emote_generations());
        if let Some(generations) = emote_generations {
            self.ensure_emote_search_index(generations);
        }
        self.suggestions = match self.state.ac_type {
            AutocompleteType::Emote => self.get_emote_suggestions(),
            AutocompleteType::Mention => self.get_mention_suggestions(),
            AutocompleteType::Command => self.get_command_suggestions(),
            AutocompleteType::Tab => self.get_tab_suggestions(),
        };
        self.prefetch_visible_suggestions(0, MAX_VISIBLE_SUGGESTIONS);
        self.advance_suggestion_revision();
        self.suggestions_scale_bits =
            crate::components::chat::canvas_renderer::layout_scale::current_bits();
        if emote_backed {
            self.last_emote_generations = emote_generations;
        }
    }

    pub(crate) fn prefetch_visible_suggestions(&self, first: usize, count: usize) {
        let mut stills = Vec::new();
        for suggestion in self
            .suggestions
            .iter()
            .skip(first)
            .take(count.min(MAX_VISIBLE_SUGGESTIONS + 1))
        {
            let Some(url) = suggestion.image_url.as_deref() else {
                continue;
            };
            if suggestion.is_animated {
                if image_cache::get_current_animated_render_image(url).is_none() {
                    image_cache::trigger_animated_decode_media_with_source(
                        url,
                        suggestion.media_identity.clone(),
                        image_cache::PreloadSource::AutocompleteVisible,
                    );
                }
            } else if image_cache::get_cached_render_image(url).is_none() {
                stills.push(image_cache::MediaRequest::new(
                    url,
                    suggestion.media_identity.clone(),
                ));
            }
        }
        if !stills.is_empty() {
            image_cache::trigger_preload_media_with_source(
                stills,
                image_cache::PreloadSource::AutocompleteVisible,
            );
        }
    }

    /// The emote an accepted completion inserted, from the search index when
    /// it covers `channel`, otherwise from the live lookup maps.
    pub fn accepted_emote(
        &self,
        channel: &str,
        name: &str,
        image_url: Option<&str>,
    ) -> Option<Arc<backend::Emote>> {
        if self.emote_index_channel.eq_ignore_ascii_case(channel)
            && let Some(emote) = self
                .emote_search_index
                .as_ref()
                .and_then(|index| index.find_code(name, image_url))
        {
            return Some(Arc::clone(emote));
        }
        crate::backend::emotes::get_emote_fast_case_sensitive(channel, name)
            .map(|emote| Arc::new(backend::Emote::from(emote)))
    }

    fn advance_suggestion_revision(&mut self) {
        self.suggestion_revision = self.suggestion_revision.wrapping_add(1);
    }

    fn current_emote_generations(&self) -> (u32, u32, u64, u64) {
        let channel = if self.state.channel.is_empty() {
            0
        } else {
            crate::backend::emotes::get_emote_cache_version(&self.state.channel)
        };
        let global = crate::backend::emotes::get_global_emote_version();
        let user = crate::state::VersionRegistry::user_emote_version();
        let layout = crate::backend::settings::get_layout_settings_version();
        (channel, global, user, layout)
    }

    fn ensure_emote_search_index(&mut self, generations: (u32, u32, u64, u64)) {
        if self.state.channel.is_empty() {
            self.emote_search_index = None;
            self.emote_index_channel.clear();
            self.emote_index_generations = Some(generations);
            return;
        }

        let include_emoji = self.state.ac_type == AutocompleteType::Emote
            && crate::backend::emotes::emote_provider_enabled(EmoteProvider::Emoji);
        if self.emote_index_generations == Some(generations)
            && self.emote_index_includes_emoji == include_emoji
            && self.emote_index_channel == self.state.channel
            && self.emote_search_index.is_some()
        {
            return;
        }

        let mut emotes = backend::get_all_emotes(&self.state.channel);
        if include_emoji {
            emotes.extend(build_picker_emoji_emotes(
                crate::backend::settings::get_emoji_style(),
            ));
        }
        self.emote_index_includes_emoji = include_emoji;
        self.emote_search_index = Some(EmoteSearchIndex::new(&self.state.channel, emotes));
        self.emote_index_channel.clone_from(&self.state.channel);
        self.emote_index_generations = Some(generations);
    }

    fn get_emote_suggestions(&self) -> Vec<AutocompleteSuggestion> {
        if self.state.channel.is_empty() {
            return vec![];
        }

        let Some(index) = self.emote_search_index.as_ref() else {
            return vec![];
        };

        index
            .rank_top(&self.state.query, MAX_SUGGESTIONS)
            .into_iter()
            .map(emote_suggestion_from_ranked)
            .collect()
    }

    fn get_mention_suggestions(&self) -> Vec<AutocompleteSuggestion> {
        if self.state.channel.is_empty() {
            return vec![];
        }

        let query = self.state.query.to_lowercase();
        let limit = if query.is_empty() {
            MAX_SUGGESTIONS
        } else {
            usize::MAX
        };
        let mut chatters = backend::irc::get_ranked_chatters(&self.state.channel, &query, limit);
        let broadcaster_login = self
            .state
            .channel
            .trim_start_matches('#')
            .to_ascii_lowercase();
        if !broadcaster_login.is_empty()
            && (query.is_empty() || broadcaster_login.starts_with(&query))
            && !chatters
                .iter()
                .any(|(login, _, _)| login.eq_ignore_ascii_case(&broadcaster_login))
        {
            chatters.push((
                broadcaster_login.clone(),
                self.state.channel.trim_start_matches('#').to_string(),
                None,
            ));
        }

        prioritize_exact_chatter_matches(&mut chatters, &query);

        let total = chatters.len();
        chatters
            .into_iter()
            .enumerate()
            .take(MAX_SUGGESTIONS)
            .map(|(index, (login, display, color))| {
                let display = if display.trim().is_empty() {
                    login.clone()
                } else {
                    display
                };
                AutocompleteSuggestion {
                    text: format!("@{display}"),
                    image_url: None,
                    subtitle: (!login.eq_ignore_ascii_case(&display)).then(|| format!("@{login}")),
                    color,
                    provider: None,
                    emote_type: None,
                    is_animated: false,
                    media_identity: None,
                    score: total.saturating_sub(index) as f64,
                }
            })
            .collect()
    }

    fn get_command_suggestions(&self) -> Vec<AutocompleteSuggestion> {
        if !crate::backend::settings::get_slash_commands_enabled()
            || !crate::backend::settings::get_command_autocomplete_enabled()
        {
            return vec![];
        }

        let show_descriptions = crate::backend::settings::get_show_command_descriptions();
        let query = &self.state.query;
        if let Some((command, option_prefix)) = command_option_query_parts(query) {
            return get_command_option_suggestions(command, option_prefix, show_descriptions);
        }
        if let Some((command, user_prefix)) = command_user_query_parts(query) {
            return self.get_command_user_suggestions(command, user_prefix, show_descriptions);
        }
        if let Some((command, channel_prefix)) = command_channel_query_parts(query) {
            return self.get_command_channel_suggestions(
                command,
                channel_prefix,
                show_descriptions,
            );
        }
        if let Some((command, message_prefix)) = command_message_query_parts(query) {
            return self.get_command_message_suggestions(
                command,
                message_prefix,
                show_descriptions,
            );
        }
        if let Some((command, outcome_prefix)) = command_prediction_outcome_query_parts(query) {
            return self.get_command_prediction_outcome_suggestions(
                command,
                outcome_prefix,
                show_descriptions,
            );
        }
        if let Some((command, value_prefix)) = command_value_query_parts(query) {
            return get_command_value_suggestions(command, value_prefix, show_descriptions);
        }

        let scored: Vec<_> = COMMAND_AUTOCOMPLETE_SPECS
            .iter()
            .filter_map(|command| {
                fuzzy_score(query, command.name).map(|score| AutocompleteSuggestion {
                    text: format!("/{}", command.name),
                    image_url: None,
                    subtitle: command_subtitle(command, show_descriptions),
                    color: None,
                    provider: None,
                    emote_type: None,
                    is_animated: false,
                    media_identity: None,
                    score,
                })
            })
            .collect();

        rank_command_suggestions(scored, query)
    }

    fn get_command_message_suggestions(
        &self,
        command: &str,
        message_prefix: &str,
        show_descriptions: bool,
    ) -> Vec<AutocompleteSuggestion> {
        if self.state.channel.is_empty() || !command_uses_message_completion(command) {
            return vec![];
        }

        let messages = backend::get_messages(&self.state.channel, 50);
        let scored: Vec<_> = messages
            .into_iter()
            .rev()
            .enumerate()
            .filter(|(_, message)| {
                !message.flags.is_system && !message.flags.is_deleted && !message.id.is_empty()
            })
            .filter_map(|(index, message)| {
                let id = message.id.to_string();
                let score = if message_prefix.is_empty() {
                    Some(1000.0 - index as f64)
                } else {
                    fuzzy_score(message_prefix, &id)
                        .map(|score| score + (50 - index.min(50)) as f64 * 0.01)
                }?;

                Some(AutocompleteSuggestion {
                    text: id,
                    image_url: None,
                    subtitle: command_message_subtitle(&message, show_descriptions),
                    color: message.color.map(|color| color.to_string()),
                    provider: None,
                    emote_type: None,
                    is_animated: false,
                    media_identity: None,
                    score,
                })
            })
            .collect();

        rank_suggestions(scored)
    }

    fn get_command_prediction_outcome_suggestions(
        &self,
        command: &str,
        outcome_prefix: &str,
        show_descriptions: bool,
    ) -> Vec<AutocompleteSuggestion> {
        if self.state.channel.is_empty() || !command_uses_prediction_outcome_completion(command) {
            return vec![];
        }

        let normalized_prefix = outcome_prefix.trim_matches('"');
        let scored: Vec<_> = backend::get_active_prediction_outcomes(&self.state.channel)
            .into_iter()
            .enumerate()
            .filter(|(_, outcome)| !outcome.title.trim().is_empty())
            .filter_map(|(index, outcome)| {
                fuzzy_score(normalized_prefix, &outcome.title).map(|score| AutocompleteSuggestion {
                    text: command_argument_value_for_replacement(&outcome.title),
                    image_url: None,
                    subtitle: command_prediction_outcome_subtitle(
                        command,
                        index + 1,
                        outcome.users,
                        outcome.channel_points,
                        show_descriptions,
                    ),
                    color: None,
                    provider: None,
                    emote_type: None,
                    is_animated: false,
                    media_identity: None,
                    score,
                })
            })
            .collect();

        rank_suggestions(scored)
    }

    fn get_command_channel_suggestions(
        &self,
        command: &str,
        channel_prefix: &str,
        show_descriptions: bool,
    ) -> Vec<AutocompleteSuggestion> {
        if !command_uses_channel_completion(command) {
            return vec![];
        }

        let normalized_prefix = channel_prefix.trim_start_matches('#');
        let mut channels = Vec::new();
        push_unique_channel(&mut channels, &self.state.channel, "Current channel");
        for channel in crate::backend::settings::get_recent_channels() {
            push_unique_channel(&mut channels, &channel, "Recent channel");
        }

        let scored: Vec<_> = channels
            .into_iter()
            .filter_map(|(channel, source)| {
                fuzzy_score(normalized_prefix, &channel).map(|score| AutocompleteSuggestion {
                    text: channel,
                    image_url: None,
                    subtitle: command_channel_subtitle(command, source, show_descriptions),
                    color: None,
                    provider: None,
                    emote_type: None,
                    is_animated: false,
                    media_identity: None,
                    score,
                })
            })
            .collect();

        rank_suggestions(scored)
    }

    fn get_command_user_suggestions(
        &self,
        command: &str,
        user_prefix: &str,
        show_descriptions: bool,
    ) -> Vec<AutocompleteSuggestion> {
        if self.state.channel.is_empty() || !command_uses_user_completion(command) {
            return vec![];
        }

        let normalized_prefix = user_prefix.trim_start_matches('@');
        let limit = if normalized_prefix.is_empty() {
            MAX_SUGGESTIONS
        } else {
            usize::MAX
        };
        let mut chatters =
            backend::irc::get_ranked_chatters(&self.state.channel, normalized_prefix, limit);
        prioritize_exact_chatter_matches(&mut chatters, normalized_prefix);
        let total = chatters.len();
        chatters
            .into_iter()
            .enumerate()
            .take(MAX_SUGGESTIONS)
            .map(|(index, (login, display, color))| AutocompleteSuggestion {
                text: login.clone(),
                image_url: None,
                subtitle: command_user_subtitle(command, &login, &display, show_descriptions),
                color,
                provider: None,
                emote_type: None,
                is_animated: false,
                media_identity: None,
                score: total.saturating_sub(index) as f64,
            })
            .collect()
    }

    /// Tab completion combines emotes and mentions (like SolidJS)
    fn get_tab_suggestions(&self) -> Vec<AutocompleteSuggestion> {
        if self.state.channel.is_empty() || self.state.query.is_empty() {
            return vec![];
        }

        let mut emotes = self.get_emote_suggestions();

        let mut mentions = self.get_mention_suggestions();

        emotes.append(&mut mentions);
        rank_suggestions(emotes)
    }

    /// Confirm selection and close the autocomplete
    /// Returns the selected text if any
    pub fn confirm_selection(&mut self, cx: &mut Context<Self>) -> Option<String> {
        if let Some(suggestion) = self.suggestions.get(self.state.selected_index) {
            let text = suggestion.text.clone();
            if let Some(callback) = &self.on_select {
                callback(text.clone());
            }
            self.state.close();
            cx.notify();
            Some(text)
        } else {
            None
        }
    }
}

impl Render for Autocomplete {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let _scale_scope =
            crate::components::chat::canvas_renderer::layout_scale::enter(window.scale_factor());
        if self.state.is_open
            && self.suggestions_scale_bits
                != crate::components::chat::canvas_renderer::layout_scale::current_bits()
        {
            self.last_emote_generations = None;
            self.refresh_open_emote_suggestions();
        }
        if !self.state.is_open || self.suggestions.is_empty() {
            return div().into_any_element();
        }

        let suggestions = self.suggestions.clone();
        let selected_idx = self.state.selected_index;
        let ac_type = self.state.ac_type;
        let viewport = window.viewport_size();
        let desired_height = (suggestions.len().min(MAX_VISIBLE_SUGGESTIONS) as f32 * ITEM_HEIGHT)
            .min(POPUP_MAX_HEIGHT);
        let popup_fit =
            crate::utils::fit_overlay_to_viewport(viewport, POPUP_WIDTH, desired_height, 8.0);
        let left = crate::utils::clamp_overlay_axis(
            self.state.x,
            popup_fit.width,
            f32::from(viewport.width),
            popup_fit.margin_x,
        );
        let top = crate::utils::clamp_overlay_axis(
            self.state.y,
            popup_fit.height,
            f32::from(viewport.height),
            popup_fit.margin_y,
        );

        div()
            .id("autocomplete")
            .absolute()
            .left(px(left))
            .top(px(top))
            .w(px(popup_fit.width))
            .max_h(px(popup_fit.height))
            .bg(colors::bg_elevated())
            .rounded(Radii::md())
            .border_1()
            .border_color(colors::border())
            .shadow_lg()
            .overflow_y_scroll()
            .track_scroll(&self.scroll_handle)
            .overflow_x_hidden()
            .children(suggestions.iter().enumerate().map(|(idx, suggestion)| {
                let is_selected = idx == selected_idx;
                render_suggestion_item(suggestion, is_selected, ac_type, idx, cx)
            }))
            .into_any_element()
    }
}

/// Render a single suggestion item
fn render_suggestion_item(
    suggestion: &AutocompleteSuggestion,
    is_selected: bool,
    ac_type: AutocompleteType,
    idx: usize,
    cx: &mut Context<Autocomplete>,
) -> impl IntoElement + use<> {
    let text = suggestion.text.clone();
    let image_url = suggestion.image_url.clone();
    let subtitle = suggestion.subtitle.clone();
    let color = suggestion.color.clone();
    let provider = suggestion.provider.clone();
    let emote_type = suggestion.emote_type;

    div()
        .id(("suggestion", idx))
        .w_full()
        .h(px(ITEM_HEIGHT))
        .pl(px(Spacing::SM_PLUS))
        .pr(px(Spacing::SM))
        .relative()
        .cursor_pointer()
        .when(is_selected, |s| {
            s.bg(colors::accent_transparent(0.12)).child(
                div()
                    .absolute()
                    .left_0()
                    .top(px(4.0))
                    .bottom(px(4.0))
                    .w(px(2.0))
                    .rounded(Radii::xs())
                    .bg(colors::accent()),
            )
        })
        .hover(|s| s.bg(colors::bg_hover()))
        .when(idx > 0, |s| {
            s.border_t_1().border_color(colors::border_subtle())
        })
        .on_activate_without_tab_stop(cx.listener(move |this, _, _window, cx| {
            this.state.selected_index = idx;
            this.confirm_selection(cx);
        }))
        .flex()
        .items_center()
        .gap(px(Spacing::SM))
        .child(render_suggestion_icon(ac_type, image_url, color.clone()))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .overflow_hidden()
                .child(
                    div()
                        .text_sm()
                        .font_weight(if is_selected {
                            FontWeight::MEDIUM
                        } else {
                            FontWeight::NORMAL
                        })
                        .text_color(
                            color
                                .as_ref()
                                .and_then(|c| crate::utils::color::parse_hex_color_opt(c))
                                .unwrap_or_else(colors::text_primary),
                        )
                        .overflow_hidden()
                        .text_ellipsis()
                        .whitespace_nowrap()
                        .child(text),
                )
                .when_some(subtitle, |s, sub| {
                    s.child(
                        div()
                            .text_xs()
                            .text_color(colors::text_muted())
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .child(sub),
                    )
                }),
        )
        .when_some(provider, |s, prov| {
            s.child(render_suggestion_origin_label(&prov, emote_type))
        })
}

/// Render provider and emote scope as one quiet label.
fn render_suggestion_origin_label(
    provider: &str,
    emote_type: Option<EmoteType>,
) -> impl IntoElement + use<> {
    let label = emote_type
        .map(|emote_type| format!("{provider} · {}", emote_type.label()))
        .unwrap_or_else(|| provider.to_string());

    div()
        .flex_shrink_0()
        .max_w(px(96.0))
        .text_size(px(FontSize::XS))
        .text_color(colors::text_muted())
        .truncate()
        .child(label)
}

/// Render suggestion icon based on type
fn render_suggestion_icon(
    ac_type: AutocompleteType,
    image_url: Option<String>,
    color: Option<String>,
) -> impl IntoElement {
    if let Some(url) = image_url {
        div()
            .w(px(EMOTE_SIZE))
            .h(px(EMOTE_SIZE))
            .flex_shrink_0()
            .child(
                img(SharedString::from(url))
                    .w(px(EMOTE_SIZE))
                    .h(px(EMOTE_SIZE))
                    .object_fit(ObjectFit::Contain),
            )
    } else {
        let icon = match ac_type {
            AutocompleteType::Emote | AutocompleteType::Tab => "😀",
            AutocompleteType::Mention => "@",
            AutocompleteType::Command => "/",
        };
        let text_color = if let Some(hex) = color {
            parse_hex_color_opt(&hex).unwrap_or(colors::text_muted())
        } else {
            colors::text_muted()
        };

        div()
            .w(px(ICON_SIZE))
            .h(px(ICON_SIZE))
            .flex_shrink_0()
            .flex()
            .items_center()
            .justify_center()
            .text_sm()
            .text_color(text_color)
            .child(icon)
    }
}

fn emote_suggestion_from_ranked(ranked: RankedEmote) -> AutocompleteSuggestion {
    let emote_type = match ranked.scope {
        EmoteScope::Channel => EmoteType::Channel,
        EmoteScope::Personal => EmoteType::Personal,
        EmoteScope::Subscription => EmoteType::Sub,
        EmoteScope::Global | EmoteScope::Emoji => EmoteType::Global,
    };
    let emote = ranked.emote;
    let image_url = emote_display_url_at_height(&emote, EMOTE_SIZE).to_string();
    let media_identity = crate::backend::types::media_source_id_for_url(&emote, &image_url);

    AutocompleteSuggestion {
        text: emote.name.to_string(),
        image_url: Some(image_url),
        subtitle: None,
        color: None,
        provider: Some(provider_name(&emote.provider)),
        emote_type: Some(emote_type),
        is_animated: emote.is_animated,
        media_identity,
        score: ranked.fuzzy_score,
    }
}

/// Get provider name
fn provider_name(provider: &EmoteProvider) -> String {
    match provider {
        EmoteProvider::Twitch => "Twitch".to_string(),
        EmoteProvider::SevenTV => "7TV".to_string(),
        EmoteProvider::BTTV => "BTTV".to_string(),
        EmoteProvider::FFZ => "FFZ".to_string(),
        EmoteProvider::Emoji => "Emoji".to_string(),
    }
}

/// Helper to detect autocomplete triggers
pub struct AutocompleteHelper;

impl AutocompleteHelper {
    /// Detect autocomplete trigger from text
    /// Returns the query and exact replacement context if a trigger is found.
    pub fn detect_trigger(text: &str, cursor_position: usize) -> Option<CompletionContext> {
        let token = echo_autocomplete_core::scan_completion_token(text, cursor_position);
        if token.before_cursor.is_empty() {
            return None;
        }

        let command_autocomplete_enabled = crate::backend::settings::get_slash_commands_enabled()
            && crate::backend::settings::get_command_autocomplete_enabled();
        if command_autocomplete_enabled {
            if token.start == 0
                && let Some(query) = token.fragment.strip_prefix('/')
                && echo_autocomplete_core::is_command_root_query(query)
            {
                return echo_autocomplete_core::completion_context(
                    text,
                    AutocompleteType::Command,
                    query.to_string(),
                    token.start..token.end,
                );
            }

            if let Some(command_context) = command_argument_context(token.before_cursor) {
                let (start, end) = command_argument_token_range(token.before_cursor)?;
                return echo_autocomplete_core::completion_context(
                    text,
                    AutocompleteType::Command,
                    command_context.query(),
                    start..end,
                );
            }
        }

        if let Some(query) = token.fragment.strip_prefix(':') {
            return echo_autocomplete_core::completion_context(
                text,
                AutocompleteType::Emote,
                query.to_string(),
                token.start..token.end,
            );
        }

        if let Some(query) = token.fragment.strip_prefix('@') {
            return echo_autocomplete_core::completion_context(
                text,
                AutocompleteType::Mention,
                query.to_string(),
                token.start..token.end,
            );
        }

        None
    }

    /// Detect a plain non-whitespace token at the cursor for Tab completion.
    pub fn detect_tab_context(text: &str, cursor_position: usize) -> Option<CompletionContext> {
        echo_autocomplete_core::detect_tab_context(text, cursor_position)
    }
}

fn command_subtitle(command: &CommandAutocompleteSpec, show_descriptions: bool) -> Option<String> {
    echo_autocomplete_core::command_subtitle(command, show_descriptions)
}

fn command_argument_context(before_cursor: &str) -> Option<CommandArgumentContext> {
    echo_autocomplete_core::command_argument_context(before_cursor, command_entry)
}

fn command_argument_token_range(before_cursor: &str) -> Option<(usize, usize)> {
    echo_autocomplete_core::command_argument_token_range(before_cursor, command_entry)
}

fn command_option_query_parts(query: &str) -> Option<(&str, &str)> {
    let (command, option_prefix) = query.split_once(' ')?;
    if option_prefix.starts_with(COMMAND_CHANNEL_QUERY_PREFIX)
        || option_prefix.starts_with(COMMAND_MESSAGE_QUERY_PREFIX)
        || option_prefix.starts_with(COMMAND_PREDICTION_OUTCOME_QUERY_PREFIX)
        || option_prefix.starts_with(COMMAND_USER_QUERY_PREFIX)
        || option_prefix.starts_with(COMMAND_VALUE_QUERY_PREFIX)
    {
        return None;
    }
    command_option_specs(command)?;
    Some((command, option_prefix))
}

fn command_channel_query_parts(query: &str) -> Option<(&str, &str)> {
    let (command, channel_query) = query.split_once(' ')?;
    let channel_prefix = channel_query.strip_prefix(COMMAND_CHANNEL_QUERY_PREFIX)?;
    command_uses_channel_completion(command).then_some((command, channel_prefix))
}

fn command_user_query_parts(query: &str) -> Option<(&str, &str)> {
    let (command, user_query) = query.split_once(' ')?;
    let user_prefix = user_query.strip_prefix(COMMAND_USER_QUERY_PREFIX)?;
    command_uses_user_completion(command).then_some((command, user_prefix))
}

fn command_message_query_parts(query: &str) -> Option<(&str, &str)> {
    let (command, message_query) = query.split_once(' ')?;
    let message_prefix = message_query.strip_prefix(COMMAND_MESSAGE_QUERY_PREFIX)?;
    command_uses_message_completion(command).then_some((command, message_prefix))
}

fn command_prediction_outcome_query_parts(query: &str) -> Option<(&str, &str)> {
    let (command, outcome_query) = query.split_once(' ')?;
    let outcome_prefix = outcome_query.strip_prefix(COMMAND_PREDICTION_OUTCOME_QUERY_PREFIX)?;
    command_uses_prediction_outcome_completion(command).then_some((command, outcome_prefix))
}

fn command_value_query_parts(query: &str) -> Option<(&str, &str)> {
    let (command, value_query) = query.split_once(' ')?;
    let value_prefix = value_query.strip_prefix(COMMAND_VALUE_QUERY_PREFIX)?;
    command_value_specs(command).map(|_| (command, value_prefix))
}

fn rank_suggestions(mut suggestions: Vec<AutocompleteSuggestion>) -> Vec<AutocompleteSuggestion> {
    suggestions.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    suggestions.truncate(MAX_SUGGESTIONS);
    suggestions
}

fn rank_command_suggestions(
    mut suggestions: Vec<AutocompleteSuggestion>,
    query: &str,
) -> Vec<AutocompleteSuggestion> {
    suggestions.sort_by(|left, right| {
        command_matches_exact_query(&right.text, query)
            .cmp(&command_matches_exact_query(&left.text, query))
            .then_with(|| {
                right
                    .score
                    .partial_cmp(&left.score)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
    });
    suggestions.truncate(MAX_SUGGESTIONS);
    suggestions
}

fn command_matches_exact_query(command: &str, query: &str) -> bool {
    !query.is_empty()
        && command
            .strip_prefix('/')
            .is_some_and(|command| command.eq_ignore_ascii_case(query))
}

fn prioritize_exact_chatter_matches(
    chatters: &mut [(String, String, Option<String>)],
    query: &str,
) {
    if query.is_empty() {
        return;
    }

    chatters.sort_by_key(|(login, display, _)| {
        !(chatter_matches_exact_query(login, query) || chatter_matches_exact_query(display, query))
    });
}

fn chatter_matches_exact_query(value: &str, query: &str) -> bool {
    value.eq_ignore_ascii_case(query)
        || (!value.is_ascii() && value.to_lowercase() == query.to_lowercase())
}

fn get_command_option_suggestions(
    command: &str,
    option_prefix: &str,
    show_descriptions: bool,
) -> Vec<AutocompleteSuggestion> {
    let Some(options) = command_option_specs(command) else {
        return vec![];
    };

    let scored: Vec<_> = options
        .iter()
        .filter_map(|(name, description)| {
            fuzzy_score(option_prefix, name).map(|score| AutocompleteSuggestion {
                text: name.to_string(),
                image_url: None,
                subtitle: if show_descriptions {
                    Some(description.to_string())
                } else {
                    None
                },
                color: None,
                provider: None,
                emote_type: None,
                is_animated: false,
                media_identity: None,
                score,
            })
        })
        .collect();

    rank_suggestions(scored)
}

fn get_command_value_suggestions(
    command: &str,
    value_prefix: &str,
    show_descriptions: bool,
) -> Vec<AutocompleteSuggestion> {
    let Some(values) = command_value_specs(command) else {
        return vec![];
    };

    let scored: Vec<_> = values
        .iter()
        .filter_map(|(value, description)| {
            fuzzy_score(value_prefix, value).map(|score| AutocompleteSuggestion {
                text: value.to_string(),
                image_url: None,
                subtitle: if show_descriptions {
                    Some(format!("{description} for /{command}"))
                } else {
                    None
                },
                color: None,
                provider: None,
                emote_type: None,
                is_animated: false,
                media_identity: None,
                score,
            })
        })
        .collect();

    rank_suggestions(scored)
}

fn command_option_specs(command: &str) -> Option<&'static [(&'static str, &'static str)]> {
    let options = command_entry(command)?.options;
    if options.is_empty() {
        None
    } else {
        Some(options)
    }
}

fn command_value_specs(command: &str) -> Option<&'static [CommandValueSpec]> {
    match command_entry(command)?.argument_completion {
        CommandArgumentCompletion::Values(values) if !values.is_empty() => Some(values),
        _ => None,
    }
}

fn command_uses_channel_completion(command: &str) -> bool {
    command_entry(command).is_some_and(|entry| {
        matches!(
            entry.argument_completion,
            CommandArgumentCompletion::Channel | CommandArgumentCompletion::UserThenChannel
        )
    })
}

fn command_uses_user_completion(command: &str) -> bool {
    command_entry(command).is_some_and(|entry| {
        matches!(
            entry.argument_completion,
            CommandArgumentCompletion::User | CommandArgumentCompletion::UserThenChannel
        )
    })
}

fn command_uses_message_completion(command: &str) -> bool {
    command_entry(command)
        .is_some_and(|entry| entry.argument_completion == CommandArgumentCompletion::MessageId)
}

fn command_uses_prediction_outcome_completion(command: &str) -> bool {
    command_entry(command).is_some_and(|entry| {
        entry.argument_completion == CommandArgumentCompletion::PredictionOutcome
    })
}

fn command_message_subtitle(
    message: &crate::backend::ChatMessage,
    show_descriptions: bool,
) -> Option<String> {
    if !show_descriptions {
        return None;
    }

    let author = if message.display_name.is_empty() {
        message.sender.as_ref()
    } else {
        message.display_name.as_ref()
    };
    let content = crate::utils::truncate_text(message.content.as_ref(), 80);
    if content.is_empty() {
        Some(format!("@{author}"))
    } else {
        Some(format!("@{author}: {content}"))
    }
}

fn push_unique_channel(
    channels: &mut Vec<(String, &'static str)>,
    channel: &str,
    source: &'static str,
) {
    let normalized = channel.trim().trim_start_matches('#').to_lowercase();
    if normalized.is_empty()
        || channels
            .iter()
            .any(|(existing, _)| existing.eq_ignore_ascii_case(&normalized))
    {
        return;
    }

    channels.push((normalized, source));
}

fn command_channel_subtitle(
    command: &str,
    source: &str,
    show_descriptions: bool,
) -> Option<String> {
    show_descriptions.then(|| format!("{source} for /{command}"))
}

fn command_user_subtitle(
    command: &str,
    login: &str,
    display: &str,
    show_descriptions: bool,
) -> Option<String> {
    if !show_descriptions {
        return None;
    }

    if display.eq_ignore_ascii_case(login) {
        Some(format!("Recent chatter for /{command}"))
    } else {
        Some(format!("{display} - recent chatter for /{command}"))
    }
}

fn command_prediction_outcome_subtitle(
    command: &str,
    index: usize,
    users: Option<u64>,
    channel_points: Option<u64>,
    show_descriptions: bool,
) -> Option<String> {
    if !show_descriptions {
        return None;
    }

    let mut details = vec![format!("Outcome {index} for /{command}")];
    if let Some(users) = users.filter(|users| *users > 0) {
        details.push(format!("{users} users"));
    }
    if let Some(points) = channel_points.filter(|points| *points > 0) {
        details.push(format!("{points} points"));
    }

    Some(details.join(" - "))
}

fn command_argument_value_for_replacement(value: &str) -> String {
    echo_autocomplete_core::command_argument_value_for_replacement(value)
}

fn command_entry(name: &str) -> Option<&'static CommandAutocompleteSpec> {
    COMMAND_AUTOCOMPLETE_SPECS
        .iter()
        .find(|command| command.name.eq_ignore_ascii_case(name))
}
