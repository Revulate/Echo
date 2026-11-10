//! The composer of a chat split:
//! - Chat input box with autocomplete integration
//! - Context row (reply target / channel target)
//! - Emote picker button
//! - Send button (optional)
//! - Typed emote indicators in the context row
//! - Input text state management via InputManager
#![allow(clippy::too_many_arguments)]

use crate::backend;
use crate::backend::irc::room_state::RoomState;
use crate::cache::loader as image_cache;
use crate::components::chat::chat_settings_bar::{
    RoomStateRailPlan, active_room_modes, clear_room_state_rail_tooltips, plan_room_state_rail,
    render_room_state_popover, render_room_state_rail,
};
use crate::components::overlays::{
    AutocompleteHelper, CompletionContext, ComposerSelectionSnapshot, ContextMenuState,
    InlineTabCompletionCandidate, InlineTabCompletionEdit, InlineTabCompletionSession,
};
use crate::components::ui::ButtonExt;
use crate::components::ui::input::{
    CHAT_INPUT_CONTEXT, TabComplete, platform_caret_blink_interval,
};
use crate::config;
use crate::managers::{InputManager, OverlayManager};
use crate::state::{ReplyState, SplitId};
use crate::theme::colors;
use crate::theme::tokens::{FontSize, Radii, RadiiPixels, Spacing};
use echo_autocomplete_core::{
    CommandArgumentKind, EnterKeyDisposition, command_argument_kind_from_query,
    completion_matches_fragment, enter_key_disposition,
};
use gpui::prelude::FluentBuilder;
use gpui::*;
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};
use unicode_segmentation::UnicodeSegmentation;

enum ClipboardPaste {
    Text(String),
    // GPUI's Linux clipboard reads text and images, never file lists.
    #[cfg_attr(target_os = "linux", allow(dead_code))]
    Files(Vec<std::path::PathBuf>),
}

#[derive(Clone, Copy)]
pub(crate) enum MultilinePasteAction {
    OneLine,
    SendLines,
    Cancel,
}

pub(crate) struct MultilinePasteChoice {
    pub id: u64,
    pub channel: String,
    pub account: Option<String>,
    pub raw: String,
    pub lines: std::collections::VecDeque<String>,
    pub draft: String,
    pub cursor: usize,
    pub selection: Option<Range<usize>>,
    pub selected_index: usize,
}

pub(crate) struct MultilineSend {
    pub channel: String,
    pub account: Option<String>,
    pub remaining: std::collections::VecDeque<String>,
    pub total: usize,
    pub submitted: usize,
    pub in_flight: u64,
    pub reply: Option<ReplyState>,
}

enum CaretWakeStep {
    Stop,
    Sleep(Duration),
}

const DRAFT_PREVIEW_LIMIT: usize = 4;
const DRAFT_PREVIEW_HEIGHT_PX: f32 = 16.0;
const DRAFT_PREVIEW_MAX_IMAGE_WIDTH_PX: f32 = 24.0;
const DRAFT_PREVIEW_RAIL_WIDTH_PX: f32 = 128.0;
const DRAFT_PREVIEW_RAIL_BORDER_PX: f32 = 1.0;
const DRAFT_PREVIEW_RAIL_PADDING_PX: f32 = Spacing::XS_PLUS;
const DRAFT_PREVIEW_GAP_PX: f32 = Spacing::XS;
const DRAFT_PREVIEW_OVERFLOW_LABEL_WIDTH_PX: f32 = 24.0;
const COMPOSER_PRIMARY_TEXT_PX: f32 = FontSize::BASE;
const COMPOSER_SECONDARY_TEXT_PX: f32 = FontSize::XS;
const COMPOSER_ACTION_TEXT_PX: f32 = FontSize::SM;
const COMPOSER_LINE_HEIGHT_PX: f32 = 18.0;
const COMPOSER_MAX_VISIBLE_LINES: usize = 4;
const MAX_COMPOSER_PASTE_CHARS: usize = config::TWITCH_MESSAGE_CHAR_LIMIT * 10;
const REPLY_BAR_LABEL_TEXT_PX: f32 = FontSize::SM;
const REPLY_BAR_LABEL_LINE_PX: f32 = 15.0;
const REPLY_BAR_EXCERPT_TEXT_PX: f32 = FontSize::XS;
const REPLY_BAR_EXCERPT_LINE_PX: f32 = 14.0;
const REPLY_BAR_ACCENT_RAIL_PX: f32 = 2.0;
const REPLY_BAR_DISMISS_SIZE_PX: f32 = 28.0;
/// Upper bound on the excerpt string this bar allocates per frame. The row is
/// single-line and ellipsized, so anything past this is never painted.
const REPLY_BAR_EXCERPT_MAX_CHARS: usize = 160;
const AUTOCOMPLETE_PRIMARY_TEXT_PX: f32 = FontSize::SM;
const AUTOCOMPLETE_SECONDARY_TEXT_PX: f32 = FontSize::XS;
const AUTOCOMPLETE_EMOTE_SIZE_PX: f32 = 26.0;
const AUTOCOMPLETE_EMPTY_ROW_HEIGHT_PX: f32 = 36.0;
const AUTOCOMPLETE_VISIBLE_ROWS: usize = 6;
const AUTOCOMPLETE_POPUP_WIDTH_PX: f32 = 360.0;
/// Editable text width the composer keeps for itself before the room-state rail
/// may claim anything. Everything past this is the rail's "surplus" budget.
const COMPOSER_RESERVED_TEXT_WIDTH_PX: f32 = 140.0;

/// Vertical space one split's input surface occupies, split into the two
/// siblings that make it up.
///
/// Parents size their message canvas from `total_height` *before* rendering the
/// surface, which is what keeps the reply bar from ever painting over a canvas
/// that is still laid out at the old height. `composer_height` is the last
/// width-aware measurement (it can only change through a draft/width change,
/// never through reply state), while `reply_height` is derived synchronously
/// from the reply state the caller already holds.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct InputSurfaceMetrics {
    pub composer_height: f32,
    pub reply_height: f32,
    pub total_height: f32,
}

impl InputSurfaceMetrics {
    pub fn new(composer_height: f32, reply_visible: bool) -> Self {
        let reply_height = if reply_visible {
            config::CHAT_REPLY_BAR_HEIGHT_PX
        } else {
            0.0
        };
        Self {
            composer_height,
            reply_height,
            total_height: composer_height + reply_height,
        }
    }
}

struct DraftPreviewCacheKey {
    raw_channel: String,
    channel_key: String,
    input_text: String,
    channel_emote_generation: u64,
    global_emote_generation: u64,
    user_emote_generation: u64,
    layout_settings_version: u64,
    window_scale_bits: u32,
}

impl DraftPreviewCacheKey {
    fn matches(
        &self,
        raw_channel: &str,
        channel_key: &str,
        input_text: &str,
        channel_emote_generation: u64,
        global_emote_generation: u64,
        user_emote_generation: u64,
        layout_settings_version: u64,
        window_scale_bits: u32,
    ) -> bool {
        self.raw_channel == raw_channel
            && self.channel_key == channel_key
            && self.input_text == input_text
            && self.channel_emote_generation == channel_emote_generation
            && self.global_emote_generation == global_emote_generation
            && self.user_emote_generation == user_emote_generation
            && self.layout_settings_version == layout_settings_version
            && self.window_scale_bits == window_scale_bits
    }
}

struct DraftPreviewItem {
    element_id: SharedString,
    image_url: SharedString,
    /// Preserve the provider's explicit animation declaration all the way to
    /// the compact pre-send rail. A raw GPUI image only exposes a static frame.
    is_animated: bool,
    /// Identity-aware source key for the exact display tier. This lets the
    /// transient composer surface share the bounded media cache correctly.
    media_identity: Option<crate::cache::disk_layer::MediaSourceId>,
    width_px: f32,
}

struct DraftPreview {
    items: Vec<DraftPreviewItem>,
    overflow_count: usize,
}

struct DraftPreviewCacheEntry {
    key: DraftPreviewCacheKey,
    preview: Arc<DraftPreview>,
}

/// The composer field `render_chat_input` built.
struct RenderedComposer {
    element: AnyElement,
    /// Whether the draft preview paints animated emotes.
    has_animated_draft_previews: bool,
    /// The collapsed chat-modes popover, when open. The composer clips its own
    /// overflow, so the popover has to be rendered as a sibling of it.
    room_modes_popover: Option<AnyElement>,
}

/// What a composer listener or IME callback acts on: the app, and the
/// channel, layout path and split the composer belongs to.
#[derive(Clone)]
struct ComposerTarget {
    app: Entity<crate::app::EchoApp>,
    channel: Arc<str>,
    path: Arc<[usize]>,
    split_id: SplitId,
}

#[derive(Clone, Copy)]
struct ComposerPointerSelection {
    split_id: SplitId,
    anchor: usize,
}

struct ComposerInputHandler {
    split_id: SplitId,
    renderer: Entity<ChatInputRenderer>,
}

#[derive(Default)]
struct AutocompleteA11yTargets {
    popup_id: Option<accesskit::NodeId>,
    active_id: Option<accesskit::NodeId>,
    count: usize,
}

struct EligibilityComposer {
    channel: String,
    last_block: Option<backend::ui_actions::composer_send::EligibilityBlock>,
}

/// Width-aware composer measurement is the only text shaping the input performs
/// per frame, and the animated draft rail re-enters `render` with an unchanged
/// draft. Key the measurement on everything that can move a wrap boundary so
/// animation-only repaints reuse it instead of reshaping.
struct ComposerMetricsCacheEntry {
    input_text: String,
    available_width_bits: u32,
    font: Font,
    line_count: usize,
    grapheme_count: usize,
}

struct ComposerLayoutCacheEntry {
    input_text: String,
    font: Font,
    width: Pixels,
    layout: Arc<ComposerTextLayout>,
}

#[derive(Default)]
struct ComposerViewport {
    offset_y: f32,
    preferred_x: Option<Pixels>,
    caret_row: Option<(usize, usize)>,
}

struct ComposerVisualRow {
    range: Range<usize>,
    hard_line: usize,
    wrapped_row: usize,
    source_offset: usize,
}

struct ComposerTextLayout {
    lines: smallvec::SmallVec<[WrappedLine; 1]>,
    rows: Vec<ComposerVisualRow>,
}

impl ComposerTextLayout {
    fn new(lines: smallvec::SmallVec<[WrappedLine; 1]>) -> Self {
        let mut rows = Vec::new();
        let mut source_offset = 0;
        for (hard_line, line) in lines.iter().enumerate() {
            let mut start = source_offset;
            for (wrapped_row, end) in line
                .wrap_boundaries()
                .iter()
                .map(|boundary| {
                    line.runs()[boundary.run_ix].glyphs[boundary.glyph_ix].index + source_offset
                })
                .chain([source_offset + line.len()])
                .enumerate()
            {
                rows.push(ComposerVisualRow {
                    range: start..end,
                    hard_line,
                    wrapped_row,
                    source_offset,
                });
                start = end;
            }
            source_offset += line.len() + 1;
        }
        Self { lines, rows }
    }

    fn row_for_cursor(&self, cursor: usize, hint: Option<(usize, usize)>) -> Option<usize> {
        if let Some((index, row)) = hint
            && index == cursor
            && self
                .rows
                .get(row)
                .is_some_and(|row| row.range.start <= cursor && cursor <= row.range.end)
        {
            return Some(row);
        }
        self.rows
            .iter()
            .position(|row| row.range.start <= cursor && cursor <= row.range.end)
    }

    fn cursor_position(
        &self,
        cursor: usize,
        hint: Option<(usize, usize)>,
    ) -> Option<Point<Pixels>> {
        let row_index = self.row_for_cursor(cursor, hint)?;
        let row = &self.rows[row_index];
        let x = if cursor == row.range.start && row.wrapped_row > 0 {
            px(0.0)
        } else {
            self.lines[row.hard_line]
                .position_for_index(cursor - row.source_offset, px(COMPOSER_LINE_HEIGHT_PX))?
                .x
        };
        Some(point(x, px(row_index as f32 * COMPOSER_LINE_HEIGHT_PX)))
    }

    fn index_at(&self, row_index: usize, x: Pixels) -> usize {
        let row = &self.rows[row_index];
        let index = self.lines[row.hard_line]
            .closest_index_for_position(
                point(
                    x,
                    px((row.wrapped_row as f32 + 0.5) * COMPOSER_LINE_HEIGHT_PX),
                ),
                px(COMPOSER_LINE_HEIGHT_PX),
            )
            .unwrap_or_else(|index| index)
            + row.source_offset;
        index.clamp(row.range.start, row.range.end)
    }
}

/// Entity for rendering chat input with state management
pub struct ChatInputRenderer {
    /// Input manager (handles input text, history, reply state)
    pub input_manager: InputManager,
    send_feedback: HashMap<SplitId, backend::ui_actions::composer_send::ComposerSendState>,
    pub(crate) multiline_pastes: HashMap<SplitId, MultilinePasteChoice>,
    pub(crate) multiline_sends: HashMap<SplitId, MultilineSend>,
    /// Parsed composer emotes, bounded by active split IDs and never GPUI elements.
    draft_preview_cache: HashMap<SplitId, DraftPreviewCacheEntry>,
    /// Frozen inline Tab candidate sets, one per live split.
    /// The visible `:`/`@` suggestion bar remains a separate interaction.
    inline_tab_completion: HashMap<SplitId, InlineTabCompletionSession>,
    /// Last rendered surface metrics for each live split. The autocomplete pass
    /// follows the composer pass and consumes this exact value so its anchor
    /// cannot drift from width-aware multiline growth or the reply bar.
    input_surface_metrics: HashMap<SplitId, InputSurfaceMetrics>,
    /// Actual laid-out text viewport for each split, captured after prepaint so
    /// window-space mouse positions can map through GPUI's wrapped glyph layout.
    composer_text_bounds: HashMap<SplitId, Bounds<Pixels>>,
    composer_viewports: HashMap<SplitId, ComposerViewport>,
    composer_focus_subscriptions: HashMap<SplitId, (FocusHandle, Subscription)>,
    /// Last measured wrap/grapheme result per live split, keyed by draft, width,
    /// and font so repeated repaints of an unchanged draft skip shaping.
    composer_metrics_cache: HashMap<SplitId, ComposerMetricsCacheEntry>,
    composer_layouts: HashMap<SplitId, ComposerLayoutCacheEntry>,
    /// Reusable draft buffer. The render pass needs the draft as an owned value
    /// because caret sync, preview resolution, and measurement all take
    /// `&mut self`, so one buffer is swapped in rather than allocated per frame.
    composer_draft_scratch: String,
    /// Stable anchor for the active pointer drag. The caret is the moving edge.
    composer_pointer_selection: Option<ComposerPointerSelection>,
    /// Platform text-input ownership for each composer. GPUI asks the active
    /// split's immutable handler for UTF-16 selection/composition state after
    /// that split registers `ElementInputHandler` during paint.
    ime_targets: HashMap<SplitId, ComposerTarget>,
    ime_handlers: HashMap<SplitId, Entity<ComposerInputHandler>>,
    ime_active_split: Option<SplitId>,
    ime_marked_ranges: HashMap<SplitId, Range<usize>>,
    /// The active split exclusively owns the shared renderer's caret state.
    caret_owner: Option<SplitId>,
    /// Exact text/cursor signature used to keep the caret visible after input activity.
    caret_activity_text: String,
    caret_activity_cursor: usize,
    caret_visible: bool,
    caret_blink_interval: Option<Duration>,
    caret_next_toggle_at: Option<Instant>,
    /// At most one pending timer belongs to the current generation.
    caret_waker_running: bool,
    caret_waker_generation: u64,
    /// The compact pre-send preview rail and inline completion popup are both
    /// transient composer surfaces. They share one edge-driven wake loop so
    /// animated emotes advance without a permanent repaint source or retained
    /// decoded media.
    composer_animation_active: bool,
    composer_animation_waker_running: bool,
    composer_animation_waker_generation: u64,
    /// Animated emote URLs the active split's composer surfaces are painting.
    /// Rebuilt every render pass of that split so the wake loop can prove
    /// whether a repaint would actually show a different frame.
    composer_animation_urls: Vec<SharedString>,
    /// Frames `composer_animation_urls` displayed at the last waker repaint.
    composer_last_frame_signature: Option<u64>,
    /// Split whose collapsed chat-modes popover is open. At most one, and only
    /// while that split's rail is actually in its collapsed tier.
    room_modes_popover: Option<SplitId>,
    /// Where the press that last dismissed the popover landed. The rail button
    /// toggles on the completed click, after that press already closed the
    /// popover from outside, so the same press must not reopen it.
    room_modes_dismiss_press: Option<Point<Pixels>>,
    /// Composer targets painted in the current tab. A temporary send block only
    /// wakes these retained leaves at its next visible countdown edge.
    eligibility_composers: HashMap<SplitId, EligibilityComposer>,
    eligibility_waker_running: bool,
    eligibility_waker_generation: u64,
    eligibility_waker_task: Option<Task<()>>,
    eligibility_visibility_subscription: Option<Subscription>,
}

impl ChatInputRenderer {
    fn native_input_is_active(&self, split_id: SplitId) -> bool {
        self.ime_active_split == Some(split_id) && self.ime_targets.contains_key(&split_id)
    }

    pub fn new(_cx: &mut Context<Self>) -> Self {
        Self {
            input_manager: InputManager::new(),
            send_feedback: HashMap::new(),
            multiline_pastes: HashMap::new(),
            multiline_sends: HashMap::new(),
            draft_preview_cache: HashMap::new(),
            inline_tab_completion: HashMap::new(),
            input_surface_metrics: HashMap::new(),
            composer_text_bounds: HashMap::new(),
            composer_viewports: HashMap::new(),
            composer_focus_subscriptions: HashMap::new(),
            composer_metrics_cache: HashMap::new(),
            composer_layouts: HashMap::new(),
            composer_draft_scratch: String::new(),
            composer_pointer_selection: None,
            ime_targets: HashMap::new(),
            ime_handlers: HashMap::new(),
            ime_active_split: None,
            ime_marked_ranges: HashMap::new(),
            caret_owner: None,
            caret_activity_text: String::new(),
            caret_activity_cursor: 0,
            caret_visible: false,
            caret_blink_interval: None,
            caret_next_toggle_at: None,
            caret_waker_running: false,
            caret_waker_generation: 0,
            composer_animation_active: false,
            composer_animation_waker_running: false,
            composer_animation_waker_generation: 0,
            composer_animation_urls: Vec::new(),
            composer_last_frame_signature: None,
            room_modes_popover: None,
            room_modes_dismiss_press: None,
            eligibility_composers: HashMap::new(),
            eligibility_waker_running: false,
            eligibility_waker_generation: 0,
            eligibility_waker_task: None,
            eligibility_visibility_subscription: None,
        }
    }

    /// Drop all renderer-local state owned by splits no longer in the layout.
    pub fn cleanup_split_ids(&mut self, valid_ids: &HashSet<SplitId>) {
        self.input_manager.cleanup_split_ids(valid_ids);
        self.send_feedback
            .retain(|split_id, _| valid_ids.contains(split_id));
        self.multiline_pastes
            .retain(|split_id, _| valid_ids.contains(split_id));
        self.multiline_sends
            .retain(|split_id, _| valid_ids.contains(split_id));
        self.draft_preview_cache
            .retain(|split_id, _| valid_ids.contains(split_id));
        self.inline_tab_completion
            .retain(|split_id, _| valid_ids.contains(split_id));
        self.input_surface_metrics
            .retain(|split_id, _| valid_ids.contains(split_id));
        self.composer_text_bounds
            .retain(|split_id, _| valid_ids.contains(split_id));
        self.composer_viewports
            .retain(|split_id, _| valid_ids.contains(split_id));
        self.composer_focus_subscriptions
            .retain(|split_id, _| valid_ids.contains(split_id));
        self.composer_metrics_cache
            .retain(|split_id, _| valid_ids.contains(split_id));
        self.composer_layouts
            .retain(|split_id, _| valid_ids.contains(split_id));
        self.ime_marked_ranges
            .retain(|split_id, _| valid_ids.contains(split_id));
        self.ime_targets
            .retain(|split_id, _| valid_ids.contains(split_id));
        self.ime_handlers
            .retain(|split_id, _| valid_ids.contains(split_id));
        self.eligibility_composers
            .retain(|split_id, _| valid_ids.contains(split_id));
        if self.eligibility_composers.is_empty() {
            self.release_eligibility_waker();
        }
        if self
            .ime_active_split
            .is_some_and(|split_id| !valid_ids.contains(&split_id))
        {
            self.ime_active_split = None;
        }
        if self
            .composer_pointer_selection
            .is_some_and(|selection| !valid_ids.contains(&selection.split_id))
        {
            self.composer_pointer_selection = None;
        }
        if let Some(split_id) = self
            .room_modes_popover
            .filter(|split_id| !valid_ids.contains(split_id))
        {
            clear_room_state_rail_tooltips(split_id.as_u64());
            self.room_modes_popover = None;
        }

        self.release_composer_animation_waker();

        if self
            .caret_owner
            .is_some_and(|split_id| !valid_ids.contains(&split_id))
        {
            self.release_input_caret();
        }
    }

    fn send_state(
        &mut self,
        split_id: SplitId,
        channel: &str,
    ) -> &mut backend::ui_actions::composer_send::ComposerSendState {
        use backend::ui_actions::composer_send::ComposerSendState;
        let account = backend::ui_actions::composer_account();
        let state = self.send_feedback.entry(split_id).or_default();
        if !state.matches(channel, &account) {
            *state = ComposerSendState::new(channel.to_string(), account);
        }
        state
    }

    pub(crate) fn set_send_notice(
        &mut self,
        split_id: SplitId,
        channel: &str,
        notice: String,
        cx: &mut Context<Self>,
    ) {
        self.send_state(split_id, channel).set_notice(notice);
        cx.notify();
    }

    pub(crate) fn begin_send(
        &mut self,
        split_id: SplitId,
        channel: &str,
        cx: &mut Context<Self>,
    ) -> Option<u64> {
        let state = self.send_state(split_id, channel);
        if state.pending_count() >= 32 {
            state.set_notice("Wait for pending messages before sending more".to_string());
            cx.notify();
            return None;
        }
        let id = backend::ui_actions::next_send_submission();
        state.begin(id);
        cx.notify();
        Some(id)
    }

    pub(crate) fn finish_send(
        &mut self,
        origin: &backend::ui_actions::SendDraftOrigin,
        channel: &str,
        notice: Option<String>,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(state) = self.send_feedback.get_mut(&origin.split_id) else {
            return false;
        };
        if !state.matches(channel, &origin.account) {
            return false;
        }
        let current = origin.account == backend::ui_actions::composer_account();
        let matched = state.finish(origin.submission_id, current.then_some(notice).flatten());
        if matched {
            cx.notify();
        }
        matched && current
    }

    pub(crate) fn offer_multiline_paste(
        &mut self,
        split_id: SplitId,
        channel: &str,
        pasted: &str,
        cx: &mut Context<Self>,
    ) -> bool {
        let (pasted, truncated) = Self::paste_prefix(pasted, MAX_COMPOSER_PASTE_CHARS);
        if truncated {
            self.set_send_notice(split_id, channel, Self::paste_limit_notice(), cx);
        }
        let lines: std::collections::VecDeque<_> = pasted
            .split(['\r', '\n', '\u{2028}', '\u{2029}'])
            .filter(|line| !line.trim().is_empty())
            .map(str::to_string)
            .collect();
        if lines.len() < 2 {
            return false;
        }
        self.multiline_pastes.insert(
            split_id,
            MultilinePasteChoice {
                id: backend::ui_actions::next_send_submission(),
                channel: channel.to_string(),
                account: backend::ui_actions::composer_account(),
                raw: pasted.to_string(),
                lines,
                draft: self.input_manager.text_for_split(split_id).to_string(),
                cursor: self.input_manager.cursor_for_split(split_id),
                selection: self.input_manager.selection_for_split(split_id),
                selected_index: 0,
            },
        );
        cx.notify();
        true
    }

    fn render_multiline_paste_choice(
        &self,
        split_id: SplitId,
        channel: &str,
        app: Entity<crate::app::EchoApp>,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let choice = self.multiline_pastes.get(&split_id)?;
        if !choice.channel.eq_ignore_ascii_case(channel)
            || choice.account != backend::ui_actions::composer_account()
        {
            return None;
        }
        let id = choice.id;
        let bottom = self
            .input_surface_metrics
            .get(&split_id)
            .map_or(config::CHAT_INPUT_HEIGHT_PX, |metrics| metrics.total_height);
        let buttons = [
            (
                "Paste as one line".to_string(),
                MultilinePasteAction::OneLine,
            ),
            (
                format!("Send {} messages", choice.lines.len()),
                MultilinePasteAction::SendLines,
            ),
            ("Cancel (Esc)".to_string(), MultilinePasteAction::Cancel),
        ];
        let selected_index = choice.selected_index;
        Some(
            div()
                .id(("multiline-paste", split_id.as_u64()))
                .role(Role::ListBox)
                .aria_label("Multiline paste options")
                .absolute()
                .left_0()
                .bottom(px(bottom))
                .w(px(270.0))
                .max_w_full()
                .p_1()
                .rounded(Radii::md())
                .bg(colors::bg_elevated())
                .border_1()
                .border_color(colors::border())
                .shadow_lg()
                .flex()
                .flex_col()
                .occlude()
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .children(
                    buttons
                        .into_iter()
                        .enumerate()
                        .map(|(index, (label, action))| {
                            let app = app.clone();
                            div()
                                .id(("paste-choice", index))
                                .role(Role::ListBoxOption)
                                .aria_selected(index == selected_index)
                                .px_2()
                                .py_1()
                                .rounded(Radii::sm())
                                .text_sm()
                                .text_color(colors::text_primary())
                                .cursor_pointer()
                                .when(index == selected_index, |row| row.bg(colors::bg_hover()))
                                .hover(|row| row.bg(colors::bg_hover()))
                                .on_activate_without_tab_stop(cx.listener(move |_, _, _, cx| {
                                    let app = app.clone();
                                    cx.defer(move |cx| {
                                        app.update(cx, |app, cx| {
                                            app.resolve_multiline_paste(
                                                split_id,
                                                Some(id),
                                                action,
                                                cx,
                                            )
                                        })
                                    });
                                }))
                                .child(label)
                        }),
                )
                .child(
                    div()
                        .px_2()
                        .text_xs()
                        .text_color(colors::text_muted())
                        .child("↑/↓ choose · Enter select · Esc cancel"),
                )
                .into_any_element(),
        )
    }

    pub(crate) fn reset_inline_tab_completion(&mut self, split_id: SplitId) {
        self.inline_tab_completion.remove(&split_id);
    }

    /// Total vertical space this split's input surface needs, for the reply
    /// state the caller is about to render with.
    ///
    /// The composer term is the last width-aware measurement (drafts wrap only
    /// after they have been laid out once), but the reply term is resolved from
    /// `reply_visible` on the spot. Parents therefore reserve the reply bar in
    /// the same layout cycle that first paints it instead of one frame later.
    pub(crate) fn input_surface_metrics_for_split(
        &self,
        split_id: SplitId,
        reply_visible: bool,
    ) -> InputSurfaceMetrics {
        let composer_height = self
            .input_surface_metrics
            .get(&split_id)
            .map(|metrics| metrics.composer_height)
            .unwrap_or(config::CHAT_INPUT_HEIGHT_PX);
        InputSurfaceMetrics::new(composer_height, reply_visible)
    }

    /// Start a frozen inline completion session and apply its first candidate
    /// immediately. Shift+Tab also begins at row zero.
    pub(crate) fn begin_inline_tab_completion(
        &mut self,
        split_id: SplitId,
        context: CompletionContext,
        candidates: Vec<InlineTabCompletionCandidate>,
        reverse: bool,
    ) -> Option<InlineTabCompletionEdit> {
        self.inline_tab_completion.remove(&split_id);
        let mut session = InlineTabCompletionSession::new(context, candidates)?;
        let edit = session.next_edit(reverse);
        if !self.input_manager.replace_range_with_token(
            split_id,
            edit.replacement_range.clone(),
            &edit.expected_fragment,
            &edit.text,
        ) {
            return None;
        }

        self.reset_composer_affinity(split_id);
        self.input_manager.end_history_navigation(split_id);
        let cursor = self.input_manager.cursor_for_split(split_id);
        session.commit_applied(&edit, cursor);
        self.inline_tab_completion.insert(split_id, session);
        Some(edit)
    }

    /// Advance a previously frozen candidate set without reranking after each
    /// replacement. Invalid text/cursor state atomically cancels the session.
    pub(crate) fn advance_inline_tab_completion(
        &mut self,
        split_id: SplitId,
        reverse: bool,
    ) -> Option<InlineTabCompletionEdit> {
        let mut session = self.inline_tab_completion.remove(&split_id)?;
        let text = self.input_manager.text_for_split(split_id);
        let cursor = self.input_manager.cursor_for_split(split_id);
        if !session.matches(text, cursor) {
            return None;
        }

        let edit = session.next_edit(reverse);
        if !self.input_manager.replace_range_with_token(
            split_id,
            edit.replacement_range.clone(),
            &edit.expected_fragment,
            &edit.text,
        ) {
            return None;
        }

        self.reset_composer_affinity(split_id);
        self.input_manager.end_history_navigation(split_id);
        let cursor = self.input_manager.cursor_for_split(split_id);
        session.commit_applied(&edit, cursor);
        self.inline_tab_completion.insert(split_id, session);
        Some(edit)
    }

    /// Route the context-specific Tab action through the same production
    /// completion controllers used by mouse acceptance and continued cycling.
    /// EchoRoot binds Tab to focus traversal, so handling the more specific
    /// ChatInput action is what lets the physical key reach this composer.
    fn handle_tab_complete_action(
        &mut self,
        split_id: SplitId,
        channel: &str,
        path: &[usize],
        app: &Entity<crate::app::EchoApp>,
        reverse: bool,
        cx: &mut Context<Self>,
    ) -> bool {
        let is_active = {
            let app = app.read(cx);
            let layout = app.split_layout.read(cx);
            layout.get_active_split_path() == path
        };
        if !is_active {
            return false;
        }

        let (autocomplete_open, selected_index, suggestion_count, suggestion_revision) = {
            let app_data = app.read(cx);
            let autocomplete = app_data.overlay_manager.read(cx).autocomplete.read(cx);
            (
                autocomplete.state.is_open,
                autocomplete.state.selected_index,
                autocomplete.suggestions.len(),
                autocomplete.suggestion_revision(),
            )
        };

        if autocomplete_open {
            if reverse && suggestion_count > 0 {
                let app = app.clone();
                cx.defer(move |cx| {
                    app.update(cx, |app, cx| {
                        app.overlay_manager
                            .read(cx)
                            .autocomplete
                            .clone()
                            .update(cx, |ac, cx| {
                                if ac.state.is_open
                                    && ac.suggestion_revision() == suggestion_revision
                                    && ac.state.split_id == split_id
                                {
                                    let index = selected_index
                                        .checked_sub(1)
                                        .unwrap_or(suggestion_count - 1);
                                    ac.select_index_from_keyboard(index);
                                    cx.notify();
                                }
                            });
                        cx.notify();
                    });
                });
                cx.notify();
                return true;
            }
            let app = app.clone();
            cx.defer(move |cx| {
                app.update(cx, |app_instance, cx| {
                    if suggestion_count > 0 {
                        if app_instance.accept_autocomplete(suggestion_revision, selected_index, cx)
                        {
                            app_instance.chat_input_renderer.update(cx, |renderer, _| {
                                renderer.reset_composer_affinity(split_id)
                            });
                        }
                    } else {
                        app_instance
                            .autocomplete(cx)
                            .update(cx, |autocomplete, _| autocomplete.state.close());
                    }
                });
            });
            return true;
        }

        if self
            .advance_inline_tab_completion(split_id, reverse)
            .is_some()
        {
            cx.notify();
            return true;
        }

        let text = self
            .input_manager
            .draft(split_id)
            .cloned()
            .unwrap_or_default();
        let cursor = self.input_manager.cursor_for_split(split_id);
        let Some(context) = AutocompleteHelper::detect_tab_context(&text, cursor) else {
            return false;
        };

        let app = app.clone();
        let channel = channel.to_string();
        cx.defer(move |cx| {
            app.update(cx, |app_instance, cx| {
                app_instance.begin_inline_tab_completion(&channel, split_id, context, reverse, cx);
            });
        });
        true
    }

    fn handle_composer_escape(
        &mut self,
        split_id: SplitId,
        reply_visible: bool,
        app: &Entity<crate::app::EchoApp>,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.ime_marked_ranges.remove(&split_id).is_some() {
            self.input_manager.cancel_ime_composition(split_id);
            self.finish_native_text_edit(split_id, cx);
            return true;
        }
        let (autocomplete_open, picker_open) = {
            let overlays = app.read(cx).overlay_manager.read(cx);
            let autocomplete = overlays.autocomplete.read(cx);
            (
                autocomplete.state.is_open && autocomplete.state.split_id == split_id,
                overlays.emote_picker.read(cx).state.is_open,
            )
        };
        if !autocomplete_open && !picker_open && self.room_modes_popover == Some(split_id) {
            self.room_modes_popover = None;
            cx.notify();
            return true;
        }
        if !autocomplete_open && !picker_open && !reply_visible {
            return false;
        }
        let app = app.clone();
        cx.defer(move |cx| {
            app.update(cx, |app, cx| {
                if autocomplete_open || picker_open {
                    app.overlay_manager.update(cx, |overlays, cx| {
                        if autocomplete_open {
                            overlays.close_autocomplete(cx);
                        } else {
                            overlays.close_emote_picker(cx);
                        }
                        cx.notify();
                    });
                } else if let Some(pane) = app.split_panes.get(&split_id) {
                    pane.update(cx, |pane, _| pane.clear_reply());
                }
                app.notify_split_leaf(split_id, cx);
                cx.notify();
            });
        });
        true
    }

    fn release_input_caret(&mut self) {
        let already_released = self.caret_owner.is_none()
            && !self.caret_waker_running
            && self.caret_next_toggle_at.is_none()
            && !self.caret_visible;
        if already_released {
            return;
        }

        self.caret_waker_generation = self.caret_waker_generation.wrapping_add(1);
        self.caret_waker_running = false;
        self.caret_owner = None;
        self.caret_activity_text.clear();
        self.caret_activity_cursor = 0;
        self.caret_visible = false;
        self.caret_blink_interval = None;
        self.caret_next_toggle_at = None;
    }

    fn sync_input_caret(
        &mut self,
        split_id: SplitId,
        input_text: &str,
        cursor_position: usize,
        is_active_split: bool,
        caret_active: bool,
        cx: &mut Context<Self>,
    ) -> bool {
        if !is_active_split {
            return false;
        }
        self.input_manager
            .set_focused_split(caret_active.then_some(split_id));

        if !caret_active {
            self.release_input_caret();
            return false;
        }

        let now = Instant::now();
        let owner_changed = self.caret_owner != Some(split_id);
        let activity_changed = owner_changed
            || self.caret_activity_cursor != cursor_position
            || self.caret_activity_text != input_text;
        let phase_stale = !self.caret_waker_running
            && self
                .caret_next_toggle_at
                .is_some_and(|deadline| deadline <= now);

        if activity_changed || phase_stale {
            self.caret_owner = Some(split_id);
            self.caret_activity_cursor = cursor_position;
            self.caret_activity_text.clear();
            self.caret_activity_text.push_str(input_text);
            self.caret_visible = true;
            if activity_changed {
                self.caret_blink_interval = platform_caret_blink_interval();
            }
            self.caret_next_toggle_at = self.caret_blink_interval.map(|interval| now + interval);
        }

        self.ensure_input_caret_waker(cx);
        self.caret_visible
    }

    /// Schedule one discrete caret edge. Its repaint schedules the next edge only
    /// while the focused input remains mounted.
    fn ensure_input_caret_waker(&mut self, cx: &mut Context<Self>) {
        if self.caret_waker_running
            || self.caret_owner.is_none()
            || self.caret_blink_interval.is_none()
        {
            return;
        }

        let Some(deadline) = self.caret_next_toggle_at else {
            return;
        };

        self.caret_waker_generation = self.caret_waker_generation.wrapping_add(1);
        let generation = self.caret_waker_generation;
        self.caret_waker_running = true;
        let initial_delay = deadline.saturating_duration_since(Instant::now());

        cx.spawn(async move |this, cx| {
            let mut delay = initial_delay;
            loop {
                cx.background_executor().timer(delay).await;

                let step = this
                    .update(cx, |renderer, cx| {
                        if renderer.caret_waker_generation != generation
                            || !renderer.caret_waker_running
                            || renderer.caret_owner.is_none()
                        {
                            return CaretWakeStep::Stop;
                        }

                        let Some(deadline) = renderer.caret_next_toggle_at else {
                            renderer.caret_waker_running = false;
                            return CaretWakeStep::Stop;
                        };
                        let Some(interval) = renderer.caret_blink_interval else {
                            renderer.caret_waker_running = false;
                            renderer.caret_visible = true;
                            return CaretWakeStep::Stop;
                        };

                        let now = Instant::now();
                        if now < deadline {
                            return CaretWakeStep::Sleep(deadline - now);
                        }

                        renderer.caret_visible = !renderer.caret_visible;
                        renderer.caret_next_toggle_at = Some(now + interval);
                        renderer.caret_waker_running = false;
                        crate::backend::metrics::record_ui_notify_source("chat_input.caret_blink");
                        cx.notify();
                        CaretWakeStep::Stop
                    })
                    .unwrap_or(CaretWakeStep::Stop);

                match step {
                    CaretWakeStep::Stop => break,
                    CaretWakeStep::Sleep(next_delay) => delay = next_delay,
                }
            }
        })
        .detach();
    }

    fn active_tab_split_ids(app: &Entity<crate::app::EchoApp>, cx: &App) -> HashSet<SplitId> {
        let app = app.read(cx);
        let layout = app.split_layout.read(cx);
        let app_state = app.app_state.read(cx);
        if layout.is_unified_input_mode() {
            return app_state
                .active_tab()
                .layout
                .split_id_at_path(layout.get_active_split_path())
                .into_iter()
                .collect();
        }
        app_state
            .active_tab()
            .layout
            .collect_split_ids()
            .into_iter()
            .collect()
    }

    fn clear_eligibility_notice(&mut self, split_id: SplitId, channel: &str) {
        let account = backend::ui_actions::composer_account();
        let Some(state) = self.send_feedback.get_mut(&split_id) else {
            return;
        };
        if !state.matches(channel, &account) {
            return;
        }
        let Some(notice) = state.notice() else {
            return;
        };
        // Notices carry Twitch's own rejection prose; these phrasings describe
        // a restriction that the eligibility line reports by itself.
        let notice = notice.to_ascii_lowercase();
        if notice.contains("you are banned")
            || notice.contains("you have been banned")
            || notice.contains("sender is banned")
            || notice.contains("user is banned")
            || notice.contains("you are timed out")
            || notice.contains("talk again in")
            || notice.contains("try again in")
            || notice.contains("send messages again in")
        {
            state.clear_notice();
        }
    }

    fn sync_send_eligibility(
        &mut self,
        split_id: SplitId,
        channel: &str,
        composer_visible: bool,
        unified_input: bool,
    ) -> Option<backend::ui_actions::composer_send::EligibilityStatus> {
        if unified_input {
            self.eligibility_composers
                .retain(|known_split_id, _| *known_split_id == split_id);
        }

        if !composer_visible {
            self.eligibility_composers.remove(&split_id);
            return None;
        }

        let status = backend::irc::room_state::current_send_eligibility(channel);
        let block = status.as_ref().map(|status| status.block);
        let previous_block = {
            let entry = self
                .eligibility_composers
                .entry(split_id)
                .or_insert_with(|| EligibilityComposer {
                    channel: channel.to_string(),
                    last_block: None,
                });
            if !entry.channel.eq_ignore_ascii_case(channel) {
                entry.channel.clear();
                entry.channel.push_str(channel);
                entry.last_block = None;
            }
            entry.last_block
        };

        if previous_block.is_some() || block.is_some() {
            self.clear_eligibility_notice(split_id, channel);
        }
        if let Some(entry) = self.eligibility_composers.get_mut(&split_id) {
            entry.last_block = block;
        }
        status
    }

    fn next_eligibility_change(&mut self) -> Option<Duration> {
        self.eligibility_composers
            .values()
            .filter_map(|composer| {
                backend::irc::room_state::current_send_eligibility(&composer.channel)
                    .and_then(|status| status.next_change)
            })
            .min()
    }

    fn refresh_send_eligibility(&mut self, active_split_ids: &HashSet<SplitId>) -> Vec<SplitId> {
        self.eligibility_composers
            .retain(|split_id, _| active_split_ids.contains(split_id));

        let mut changed = Vec::new();
        let mut expired = Vec::new();
        for (split_id, composer) in &mut self.eligibility_composers {
            let block = backend::irc::room_state::current_send_eligibility(&composer.channel)
                .map(|status| status.block);
            if composer.last_block != block {
                if composer.last_block.is_some() && block.is_none() {
                    expired.push((*split_id, composer.channel.clone()));
                }
                composer.last_block = block;
                changed.push(*split_id);
            }
        }
        for (split_id, channel) in expired {
            self.clear_eligibility_notice(split_id, &channel);
        }
        changed
    }

    fn release_eligibility_waker(&mut self) {
        self.eligibility_waker_generation = self.eligibility_waker_generation.wrapping_add(1);
        self.eligibility_waker_running = false;
        self.eligibility_waker_task.take();
    }

    fn ensure_eligibility_visibility_observer(
        &mut self,
        app: &Entity<crate::app::EchoApp>,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        if self.eligibility_visibility_subscription.is_some() {
            return;
        }

        let renderer = cx.weak_entity();
        let app = app.downgrade();
        self.eligibility_visibility_subscription = Some(window.observe_window_visibility(
            move |visibility, window, cx| {
                let Some(renderer) = renderer.upgrade() else {
                    return;
                };
                let Some(app) = app.upgrade() else {
                    return;
                };
                if !visibility.is_visible() {
                    renderer.update(cx, |renderer, _| renderer.release_eligibility_waker());
                    return;
                }

                let active_split_ids = Self::active_tab_split_ids(&app, cx);
                let split_ids = renderer.update(cx, |renderer, cx| {
                    renderer.refresh_send_eligibility(&active_split_ids);
                    renderer.ensure_eligibility_waker(&app, window, cx);
                    renderer
                        .eligibility_composers
                        .keys()
                        .copied()
                        .collect::<Vec<_>>()
                });
                let unified_input = app.read(cx).split_layout.read(cx).is_unified_input_mode();
                app.update(cx, |app, cx| {
                    if unified_input {
                        cx.notify();
                    } else {
                        for split_id in split_ids {
                            app.notify_split_leaf(split_id, cx);
                        }
                    }
                });
            },
        ));
    }

    /// Wake at the next displayed countdown boundary. The task checks native
    /// window visibility before notifying retained split leaves, so hidden and
    /// permanent-ban composers never create a periodic repaint source.
    fn ensure_eligibility_waker(
        &mut self,
        app: &Entity<crate::app::EchoApp>,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        if self.eligibility_waker_running || !window.is_visible() {
            return;
        }
        let Some(delay) = self.next_eligibility_change() else {
            return;
        };

        self.eligibility_waker_generation = self.eligibility_waker_generation.wrapping_add(1);
        let generation = self.eligibility_waker_generation;
        self.eligibility_waker_running = true;
        let app = app.downgrade();
        let task = cx.spawn_in(window, async move |this, cx| {
            cx.background_executor().timer(delay).await;
            let Some(app) = app.upgrade() else {
                return;
            };
            let _ = cx.update(|window, app_cx| {
                if !window.is_visible() {
                    let _ = this.update(app_cx, |renderer, _| {
                        if renderer.eligibility_waker_generation == generation {
                            renderer.eligibility_waker_running = false;
                            renderer.eligibility_waker_task.take();
                        }
                    });
                    return;
                }

                let active_split_ids = Self::active_tab_split_ids(&app, app_cx);
                let changed = this
                    .update(app_cx, |renderer, cx| {
                        if renderer.eligibility_waker_generation != generation {
                            return Vec::new();
                        }
                        renderer.eligibility_waker_running = false;
                        renderer.eligibility_waker_task.take();
                        let changed = renderer.refresh_send_eligibility(&active_split_ids);
                        renderer.ensure_eligibility_waker(&app, window, cx);
                        changed
                    })
                    .unwrap_or_default();

                if !changed.is_empty() {
                    let unified_input = app
                        .read(app_cx)
                        .split_layout
                        .read(app_cx)
                        .is_unified_input_mode();
                    app.update(app_cx, |app, cx| {
                        if unified_input {
                            cx.notify();
                        } else {
                            for split_id in changed {
                                app.notify_split_leaf(split_id, cx);
                            }
                        }
                    });
                }
            });
        });
        self.eligibility_waker_task = Some(task);
    }

    /// Hash the animation frames the composer's visible emotes display right now.
    ///
    /// `None` means the frame set cannot be proven stable — unresolved metadata
    /// or a non-resident frame — and must fall through to an unconditional wake
    /// so the render pass keeps driving decode/refill.
    fn composer_animation_frame_signature(&self) -> Option<u64> {
        if self.composer_animation_urls.is_empty() {
            return None;
        }

        use std::hash::{Hash, Hasher};
        let time_ms = crate::cache::frame_layer::current_animation_time_ms();
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        for url in &self.composer_animation_urls {
            let url: &str = url.as_ref();
            let metadata = image_cache::get_animation_metadata(url)?;
            let frame_idx = metadata.frame_index_at(time_ms);
            if !image_cache::has_animated_frame(url, frame_idx) {
                return None;
            }
            url.hash(&mut hasher);
            frame_idx.hash(&mut hasher);
        }
        Some(hasher.finish())
    }

    /// Keep composer-owned animated emotes alive only while the active input
    /// visibly renders one. This mirrors the picker's visible-only policy
    /// without turning the composer into a permanent repaint source.
    fn sync_composer_animation_waker(
        &mut self,
        has_visible_animated_emotes: bool,
        cx: &mut Context<Self>,
    ) {
        if !crate::emote_renderer::animation_enabled() {
            self.composer_animation_active = false;
            self.composer_animation_waker_running = false;
            self.composer_animation_waker_generation =
                self.composer_animation_waker_generation.wrapping_add(1);
            self.composer_last_frame_signature = None;
            return;
        }
        if self.composer_animation_active != has_visible_animated_emotes {
            self.composer_animation_active = has_visible_animated_emotes;
            self.composer_animation_waker_generation =
                self.composer_animation_waker_generation.wrapping_add(1);
            self.composer_last_frame_signature = None;
            if !has_visible_animated_emotes {
                self.composer_animation_waker_running = false;
                return;
            }
        }

        if !has_visible_animated_emotes || self.composer_animation_waker_running {
            return;
        }

        self.composer_animation_waker_running = true;
        let generation = self.composer_animation_waker_generation;
        cx.spawn(async move |this, cx| {
            loop {
                let min_tick =
                    crate::managers::dynamic_tick_scheduler::animation_min_tick_interval_ms();
                let interval_ms = crate::emote_renderer::current_animation_interval_ms()
                    .max(min_tick)
                    .max(16);
                cx.background_executor()
                    .timer(Duration::from_millis(interval_ms))
                    .await;

                let keep_running = this
                    .update(cx, |renderer, cx| {
                        if renderer.composer_animation_waker_generation != generation
                            || !renderer.composer_animation_active
                            || !crate::emote_renderer::animation_enabled()
                        {
                            if renderer.composer_animation_waker_generation == generation {
                                renderer.composer_animation_waker_running = false;
                            }
                            return false;
                        }

                        crate::emote_renderer::tick_animation();

                        match renderer.composer_animation_frame_signature() {
                            Some(signature)
                                if renderer.composer_last_frame_signature == Some(signature) =>
                            {
                                crate::backend::metrics::record_emote_event(
                                    "chat_input.composer_animation_frame_unchanged_skipped",
                                );
                                return true;
                            }
                            signature => renderer.composer_last_frame_signature = signature,
                        }

                        crate::backend::metrics::record_ui_notify_source(
                            "chat_input.composer_animation_waker",
                        );
                        cx.notify();
                        true
                    })
                    .unwrap_or(false);

                if !keep_running {
                    break;
                }
            }
        })
        .detach();
    }

    fn release_composer_animation_waker(&mut self) {
        if !self.composer_animation_active && !self.composer_animation_waker_running {
            return;
        }
        self.composer_animation_active = false;
        self.composer_animation_waker_running = false;
        self.composer_animation_waker_generation =
            self.composer_animation_waker_generation.wrapping_add(1);
        self.composer_last_frame_signature = None;
        self.composer_animation_urls.clear();
    }

    fn draft_preview_for_split(
        &mut self,
        split_id: SplitId,
        raw_channel: &str,
        input_text: &str,
        window_scale: f32,
    ) -> Arc<DraftPreview> {
        let window_scale_bits = window_scale.to_bits();
        if let Some(entry) = self.draft_preview_cache.get_mut(&split_id)
            && entry.key.raw_channel == raw_channel
        {
            let channel_emote_generation =
                crate::backend::emotes::get_emote_cache_version(&entry.key.channel_key) as u64;
            let global_emote_generation = crate::state::VersionRegistry::global_emote_version();
            let user_emote_generation = crate::state::VersionRegistry::user_emote_version();
            let layout_settings_version = backend::settings::get_layout_settings_version();

            if entry.key.matches(
                raw_channel,
                &entry.key.channel_key,
                input_text,
                channel_emote_generation,
                global_emote_generation,
                user_emote_generation,
                layout_settings_version,
                window_scale_bits,
            ) {
                return Arc::clone(&entry.preview);
            }

            let preview = Arc::new(Self::resolve_draft_preview(
                split_id,
                &entry.key.channel_key,
                input_text,
                window_scale,
            ));
            if entry.key.input_text != input_text {
                entry.key.input_text.clear();
                entry.key.input_text.push_str(input_text);
            }
            entry.key.channel_emote_generation = channel_emote_generation;
            entry.key.global_emote_generation = global_emote_generation;
            entry.key.user_emote_generation = user_emote_generation;
            entry.key.layout_settings_version = layout_settings_version;
            entry.key.window_scale_bits = window_scale_bits;
            entry.preview = Arc::clone(&preview);
            return preview;
        }

        let channel_key = echo_core::channel_identity::normalize_channel_key(raw_channel);
        let channel_emote_generation =
            crate::backend::emotes::get_emote_cache_version(&channel_key) as u64;
        let global_emote_generation = crate::state::VersionRegistry::global_emote_version();
        let user_emote_generation = crate::state::VersionRegistry::user_emote_version();
        let layout_settings_version = backend::settings::get_layout_settings_version();

        let preview = Arc::new(Self::resolve_draft_preview(
            split_id,
            &channel_key,
            input_text,
            window_scale,
        ));
        self.draft_preview_cache.insert(
            split_id,
            DraftPreviewCacheEntry {
                key: DraftPreviewCacheKey {
                    raw_channel: raw_channel.to_string(),
                    channel_key,
                    input_text: input_text.to_string(),
                    channel_emote_generation,
                    global_emote_generation,
                    user_emote_generation,
                    layout_settings_version,
                    window_scale_bits,
                },
                preview: Arc::clone(&preview),
            },
        );
        preview
    }

    fn resolve_draft_preview(
        split_id: SplitId,
        channel_key: &str,
        input_text: &str,
        window_scale: f32,
    ) -> DraftPreview {
        let mut seen = HashSet::new();
        let mut items = Vec::with_capacity(DRAFT_PREVIEW_LIMIT);

        for word in input_text.split_whitespace() {
            let Some(emote) = crate::backend::slots::resolve_input_emote_token(channel_key, word)
            else {
                continue;
            };
            let identity = if emote.id.is_empty() {
                Arc::clone(&emote.name)
            } else {
                Arc::clone(&emote.id)
            };
            if !seen.insert((emote.provider, Arc::clone(&identity))) {
                continue;
            }

            if items.len() < DRAFT_PREVIEW_LIMIT {
                let width_px = match (emote.width, emote.height) {
                    (Some(width), Some(height)) if width > 0 && height > 0 => {
                        (DRAFT_PREVIEW_HEIGHT_PX * width as f32 / height as f32)
                            .clamp(8.0, DRAFT_PREVIEW_MAX_IMAGE_WIDTH_PX)
                    }
                    _ => DRAFT_PREVIEW_HEIGHT_PX,
                };
                let image_url = crate::backend::emotes::chat_display_url_for_scale(
                    emote.as_ref(),
                    window_scale,
                );
                items.push(DraftPreviewItem {
                    element_id: SharedString::from(format!(
                        "draft-preview-{}-{}-{}",
                        split_id.as_u64(),
                        emote.provider.stable_cache_key(),
                        identity
                    )),
                    image_url: SharedString::from(image_url.to_string()),
                    is_animated: emote.is_animated,
                    media_identity: crate::backend::emotes::media_source_id_for_url(
                        emote.as_ref(),
                        image_url,
                    ),
                    width_px,
                });
            }
        }

        DraftPreview {
            items,
            overflow_count: seen.len().saturating_sub(DRAFT_PREVIEW_LIMIT),
        }
    }

    /// Main render entry point for input area
    ///
    /// Renders the complete input area including:
    /// - Autocomplete popup (if active)
    /// - Reply context bar (sibling above the composer, reserves its own space)
    /// - Context row (send block / channel target / character counter)
    /// - Chat input field
    /// - Emote picker button
    /// - Send button (if enabled)
    ///
    /// Parameters:
    /// - `split_id`: Stable identifier for this split (input, cursor, history, reply routing)
    /// - `reply_state`: Optional reply state for this split (passed from EchoApp's SplitPane)
    /// - `room_state`: ROOMSTATE for the channel this composer sends to. In
    ///   unified mode the caller passes the *active split's* state, taken from
    ///   the same snapshot as `channel`/`split_id`/`reply_state`, so the rail
    ///   can never disagree with the target for a frame during focus changes.
    pub fn render(
        &mut self,
        channel: &str,
        path: &[usize],
        input_focus: &FocusHandle,
        is_active_split: bool,
        split_id: SplitId,
        reply_state: Option<ReplyState>,
        room_state: Option<&RoomState>,
        pane_width: f32,
        app: Entity<crate::app::EchoApp>,
        overlay_manager: Entity<OverlayManager>,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        if self.multiline_pastes.get(&split_id).is_some_and(|choice| {
            !choice.channel.eq_ignore_ascii_case(channel)
                || choice.account != backend::ui_actions::composer_account()
        }) {
            self.multiline_pastes.remove(&split_id);
        }
        let _scale_scope =
            crate::components::chat::canvas_renderer::layout_scale::enter(window.scale_factor());
        let reply_bar = reply_state
            .as_ref()
            .map(|reply| self.render_reply_context_bar(channel, split_id, reply, &app, cx));
        let autocomplete_a11y = Rc::new(RefCell::new(AutocompleteA11yTargets::default()));
        let RenderedComposer {
            element: chat_input,
            has_animated_draft_previews,
            room_modes_popover,
        } = self.render_chat_input(
            channel,
            path,
            input_focus,
            is_active_split,
            split_id,
            reply_state.is_some(),
            room_state,
            pane_width,
            app.clone(),
            Rc::clone(&autocomplete_a11y),
            window,
            cx,
        );

        let (autocomplete_element, has_animated_suggestions) = self.render_inline_autocomplete(
            channel,
            split_id,
            overlay_manager.clone(),
            app.clone(),
            Rc::clone(&autocomplete_a11y),
            cx,
        );
        let paste_choice = is_active_split
            .then(|| self.render_multiline_paste_choice(split_id, channel, app, cx))
            .flatten();

        if is_active_split {
            self.sync_composer_animation_waker(
                input_focus.is_focused(window)
                    && window.is_window_active()
                    && (has_animated_draft_previews || has_animated_suggestions),
                cx,
            );
        }

        let popover_anchor = self
            .input_surface_metrics
            .get(&split_id)
            .map(|metrics| metrics.total_height)
            .unwrap_or(config::CHAT_INPUT_HEIGHT_PX);

        div()
            .id("input-area-wrapper")
            .role(Role::Group)
            .w_full()
            .flex_shrink_0()
            .relative()
            .a11y_synthetic_children(move |builder| {
                let targets = autocomplete_a11y.borrow();
                let mut status = accesskit::Node::new(Role::Status);
                status.set_live(accesskit::Live::Polite);
                if targets.popup_id.is_some() {
                    status.set_label(format!("{} suggestions", targets.count));
                }
                let status_id = builder.synthetic_node_id("completion-status");
                builder.push_child(status_id, status);
            })
            .child(autocomplete_element)
            .children(paste_choice)
            .when_some(room_modes_popover, |wrapper, popover| {
                wrapper.child(
                    div()
                        .absolute()
                        .right(px(4.0))
                        .bottom(px(popover_anchor + 4.0))
                        .child(popover),
                )
            })
            .child(
                div()
                    .id("input-area")
                    .w_full()
                    .flex()
                    .flex_col()
                    .children(reply_bar)
                    .child(chat_input),
            )
    }

    /// Fixed-height reply context bar rendered immediately above the composer.
    ///
    /// This deliberately reserves layout space instead of floating: the parent
    /// subtracted [`InputSurfaceMetrics::total_height`] from the message canvas
    /// before this element is built, so the bar can never cover chat content.
    fn render_reply_context_bar(
        &mut self,
        channel: &str,
        split_id: SplitId,
        reply: &ReplyState,
        app: &Entity<crate::app::EchoApp>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let label = if backend::settings::get_unified_input_mode() && !channel.is_empty() {
            format!("{} · Replying to @{}", channel, reply.username)
        } else {
            format!("Replying to @{}", reply.username)
        };
        let excerpt = Self::truncate_preview_text(&reply.preview, REPLY_BAR_EXCERPT_MAX_CHARS);

        let open_thread_channel = channel.to_string();
        let open_thread_message_id = reply.message_id.clone();
        let reply_accessible_label = if excerpt.is_empty() {
            format!("{label}. Open reply thread")
        } else {
            format!("{label}: {excerpt}. Open reply thread")
        };

        div()
            .id("reply-context-bar")
            .w_full()
            .h(px(config::CHAT_REPLY_BAR_HEIGHT_PX))
            .flex_shrink_0()
            .overflow_hidden()
            .flex()
            .items_center()
            .gap(px(Spacing::XS_PLUS))
            .bg(colors::bg_elevated())
            .border_t_1()
            .border_color(colors::border_subtle())
            .child(
                div()
                    .w(px(REPLY_BAR_ACCENT_RAIL_PX))
                    .h_full()
                    .flex_shrink_0()
                    .bg(colors::accent()),
            )
            .child(
                div()
                    .id("reply-context-open-thread")
                    .role(Role::Button)
                    .aria_label(reply_accessible_label)
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .flex()
                    .flex_col()
                    .justify_center()
                    .gap_0()
                    .cursor_pointer()
                    .hover(|s| s.bg(colors::bg_hover()))
                    .on_activate({
                        let channel = open_thread_channel.clone();
                        let message_id = open_thread_message_id.clone();
                        move |event: &ClickEvent, _window, _cx| {
                            backend::push_ui_action(backend::UiAction::ViewThread {
                                message_id: message_id.clone(),
                                channel: channel.clone(),
                                x: event.position().x.into(),
                                y: event.position().y.into(),
                            });
                        }
                    })
                    .on_a11y_action(AccessibleAction::Click, {
                        let channel = open_thread_channel.clone();
                        let message_id = open_thread_message_id.clone();
                        move |_data, window, _cx| {
                            let center = window.viewport_size() / 2.;
                            backend::push_ui_action(backend::UiAction::ViewThread {
                                message_id: message_id.clone(),
                                channel: channel.clone(),
                                x: center.width.into(),
                                y: center.height.into(),
                            });
                        }
                    })
                    .child(
                        div()
                            .w_full()
                            .min_w_0()
                            .overflow_hidden()
                            .text_size(px(REPLY_BAR_LABEL_TEXT_PX))
                            .line_height(px(REPLY_BAR_LABEL_LINE_PX))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(colors::text_primary())
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .child(label),
                    )
                    .child(
                        div()
                            .w_full()
                            .min_w_0()
                            .overflow_hidden()
                            .text_size(px(REPLY_BAR_EXCERPT_TEXT_PX))
                            .line_height(px(REPLY_BAR_EXCERPT_LINE_PX))
                            .text_color(colors::text_muted())
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .child(excerpt),
                    ),
            )
            .child(
                div()
                    .id("reply-context-dismiss")
                    .role(Role::Button)
                    .aria_label("Cancel reply")
                    .w(px(REPLY_BAR_DISMISS_SIZE_PX))
                    .h(px(REPLY_BAR_DISMISS_SIZE_PX))
                    .mr(px(Spacing::XS))
                    .flex_shrink_0()
                    .rounded(Radii::sm())
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor_pointer()
                    .hover(|s| s.bg(colors::bg_hover()))
                    .on_activate({
                        let app = app.clone();
                        cx.listener(move |_this, _event: &ClickEvent, _window, cx| {
                            app.update(cx, |app, cx| {
                                if let Some(pane) = app.split_panes.get(&split_id) {
                                    pane.update(cx, |pane, _| pane.clear_reply());
                                }
                                cx.notify();
                            });
                            cx.stop_propagation();
                            cx.notify();
                        })
                    })
                    .on_a11y_action(AccessibleAction::Click, {
                        let app = app.clone();
                        move |_data, _window, cx| {
                            app.update(cx, |app, cx| {
                                if let Some(pane) = app.split_panes.get(&split_id) {
                                    pane.update(cx, |pane, _| pane.clear_reply());
                                }
                                cx.notify();
                            });
                        }
                    })
                    .child(
                        div()
                            .text_size(px(REPLY_BAR_EXCERPT_TEXT_PX))
                            .text_color(colors::text_secondary())
                            .child(crate::icons::CLOSE_GLYPH),
                    ),
            )
            .into_any_element()
    }

    /// Render the chat input field with autocomplete support.
    fn render_chat_input(
        &mut self,
        channel: &str,
        path: &[usize],
        input_focus: &FocusHandle,
        is_active_split: bool,
        split_id: SplitId,
        reply_visible: bool,
        room_state: Option<&RoomState>,
        pane_width: f32,
        app: Entity<crate::app::EchoApp>,
        autocomplete_a11y: Rc<RefCell<AutocompleteA11yTargets>>,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> RenderedComposer {
        let channel_for_input: Arc<str> = Arc::from(channel);
        let path_for_input: Arc<[usize]> = Arc::from(path);
        let target = ComposerTarget {
            app: app.clone(),
            channel: Arc::clone(&channel_for_input),
            path: Arc::clone(&path_for_input),
            split_id,
        };
        if is_active_split
            && self
                .composer_focus_subscriptions
                .get(&split_id)
                .is_none_or(|(focus, _)| focus != input_focus)
        {
            let focus = input_focus.clone();
            cx.defer_in(window, move |renderer, window, cx| {
                if !renderer.ime_targets.contains_key(&split_id) {
                    return;
                }
                let subscription = cx.on_blur(&focus, window, move |renderer, _, cx| {
                    renderer.input_manager.commit_ime_composition(split_id);
                    if renderer.ime_marked_ranges.remove(&split_id).is_some() {
                        cx.notify();
                    }
                    renderer.input_manager.set_focused_split(None);
                });
                renderer
                    .composer_focus_subscriptions
                    .insert(split_id, (focus, subscription));
            });
        }
        if is_active_split {
            if self.ime_active_split != Some(split_id)
                && let Some(previous) = self.ime_active_split.replace(split_id)
            {
                self.input_manager.commit_ime_composition(previous);
                self.ime_marked_ranges.remove(&previous);
            }
            self.ime_targets.insert(split_id, target.clone());
            if let std::collections::hash_map::Entry::Vacant(entry) =
                self.ime_handlers.entry(split_id)
            {
                let renderer = cx.entity().clone();
                let handler = cx.new(move |_| ComposerInputHandler { split_id, renderer });
                entry.insert(handler);
            }
        }
        let mut input_text = std::mem::take(&mut self.composer_draft_scratch);
        input_text.clear();
        if let Some(draft) = self.input_manager.draft(split_id) {
            input_text.push_str(draft);
        }
        let cursor_position = self.input_manager.cursor_for_split(split_id);
        let input_selection = self.input_manager.selection_for_split(split_id);
        let ime_marked_range = self.ime_marked_ranges.get(&split_id).cloned();
        let is_placeholder = input_text.is_empty();
        let show_send_button = backend::settings::get_show_send_button();
        let show_char_counter = backend::settings::get_show_char_counter();
        let is_unified_input = backend::settings::get_unified_input_mode();
        self.ensure_eligibility_visibility_observer(&app, window, cx);
        let eligibility_status = self.sync_send_eligibility(
            split_id,
            channel,
            !is_unified_input || is_active_split,
            is_unified_input,
        );
        self.ensure_eligibility_waker(&app, window, cx);
        let eligibility_block = eligibility_status.as_ref().map(|status| status.block);
        let eligibility_text = eligibility_block.map(|block| block.text(channel));
        let send_block_reason = if channel.is_empty() {
            None
        } else {
            backend::irc::ensure_authenticated_send_ready()
                .err()
                .map(|block| Self::send_block_status_text(&block))
        };
        let send_blocked = send_block_reason.is_some();
        let send_feedback = self
            .send_feedback
            .get(&split_id)
            .filter(|state| state.matches(channel, &backend::ui_actions::composer_account()))
            .and_then(|state| state.text())
            .map(|text| {
                self.multiline_sends.get(&split_id).map_or_else(
                    || text.clone(),
                    |batch| format!("{text} ({}/{})", batch.submitted, batch.total),
                )
            });
        let show_context_row = send_feedback.is_some()
            || send_blocked
            || eligibility_text.is_some()
            || is_unified_input
            || show_char_counter;

        let context_text = if !show_context_row {
            String::new()
        } else if let Some(eligibility) = eligibility_text.as_ref() {
            eligibility.clone()
        } else if let Some(feedback) = send_feedback {
            feedback
        } else if let Some(reason) = send_block_reason {
            reason.to_string()
        } else if channel.is_empty() {
            "No channel selected".to_string()
        } else if is_unified_input {
            format!("Typing in #{} (active split)", channel)
        } else {
            format!("Typing in #{}", channel)
        };

        let is_focused = input_focus.is_focused(window) && is_active_split;
        let caret_active = is_focused && window.is_window_active();
        let caret_visible = self.sync_input_caret(
            split_id,
            &input_text,
            cursor_position,
            is_active_split,
            caret_active,
            cx,
        );
        let draft_preview =
            self.draft_preview_for_split(split_id, channel, &input_text, window.scale_factor());
        let has_animated_draft_previews = draft_preview.items.iter().any(|item| item.is_animated);
        if is_active_split {
            self.composer_animation_urls.clear();
            self.composer_animation_urls.extend(
                draft_preview
                    .items
                    .iter()
                    .filter(|item| item.is_animated)
                    .map(|item| item.image_url.clone()),
            );
        }
        let input_placeholder = if !is_placeholder || send_blocked {
            String::new()
        } else if channel.is_empty() {
            "Select a channel to start chatting".to_string()
        } else {
            format!("Message #{}", channel)
        };

        let actions_reserved_width = if show_send_button { 108.0 } else { 44.0 };
        let preview_width = Self::draft_preview_width(draft_preview.as_ref());
        let composer_available_width = pane_width
            - 12.0
            - actions_reserved_width
            - preview_width
            - if preview_width > 0.0 { 4.0 } else { 0.0 };

        let room_modes = active_room_modes(room_state);
        let rail_target_label = (is_unified_input && !channel.is_empty())
            .then(|| channel.to_string())
            .filter(|_| !room_modes.is_empty());
        let rail_plan = plan_room_state_rail(
            &room_modes,
            rail_target_label.as_deref(),
            composer_available_width - COMPOSER_RESERVED_TEXT_WIDTH_PX,
        );
        let rail_width = rail_plan.map_or(0.0, |plan| plan.width);
        if self.room_modes_popover == Some(split_id)
            && !rail_plan.is_some_and(RoomStateRailPlan::is_collapsed)
        {
            self.room_modes_popover = None;
        }
        let popover_open = self.room_modes_popover == Some(split_id);

        let editable_text_width = (composer_available_width - rail_width).max(48.0);
        let (composer_line_count, input_char_count) =
            self.composer_metrics(split_id, &input_text, editable_text_width, window);
        let visible_line_count = composer_line_count.min(COMPOSER_MAX_VISIBLE_LINES);
        let composer_overflows = composer_line_count > COMPOSER_MAX_VISIBLE_LINES;
        let composer_height = Self::composer_surface_height(visible_line_count, show_context_row);
        let surface_metrics = InputSurfaceMetrics::new(composer_height, reply_visible);
        let composer_height_changed = self
            .input_surface_metrics
            .insert(split_id, surface_metrics)
            .is_none_or(|previous| (previous.composer_height - composer_height).abs() > 0.5);
        if composer_height_changed {
            let app_for_composer_layout = app.clone();
            cx.defer(move |cx| {
                app_for_composer_layout.update(cx, |_app, cx| cx.notify());
            });
        }
        let input_focus_for_click = input_focus.clone();
        let feedback_geometry_enabled =
            is_active_split && crate::bench::ui_fixture::feedback_ui_enabled();
        let composer_bounds_entity = cx.entity().clone();
        let composer_input_handler = self.ime_handlers.get(&split_id).cloned();
        let composer_input_focus = input_focus.clone();
        let register_native_input = is_active_split;
        let feedback_pointer_text_len = input_text.len();
        let feedback_pointer_selection = input_selection.clone();
        let composer_a11y_text = SharedString::from(input_text.clone());
        let composer_a11y_selection = input_selection.as_ref().map(|selection| {
            let (anchor, focus) = if cursor_position == selection.start {
                (selection.end, selection.start)
            } else {
                (selection.start, selection.end)
            };
            (
                composer_a11y_text[..anchor.min(composer_a11y_text.len())]
                    .chars()
                    .count(),
                composer_a11y_text[..focus.min(composer_a11y_text.len())]
                    .chars()
                    .count(),
            )
        });
        let composer_a11y_cursor = composer_a11y_text
            [..cursor_position.min(composer_a11y_text.len())]
            .chars()
            .count();
        let composer_a11y_label = if channel.is_empty() {
            "Chat message".to_string()
        } else {
            format!("Message #{}", channel)
        };

        let input = div()
            .on_children_prepainted(move |child_bounds, _window, _cx| {
                if !feedback_geometry_enabled {
                    return;
                }
                let Some((shell, actions)) = child_bounds.first().zip(child_bounds.get(1)) else {
                    return;
                };
                let text_height: f32 = shell.size.height.into();
                let shell_left: f32 = shell.origin.x.into();
                let shell_right: f32 = (shell.origin.x + shell.size.width).into();
                let actions_left: f32 = actions.origin.x.into();
                let actions_right: f32 = (actions.origin.x + actions.size.width).into();
                let actions_height: f32 = actions.size.height.into();
                let action_height = actions_height;
                let overlap_safe = editable_text_width >= 48.0
                    && actions_left >= shell_left
                    && actions_right <= shell_right + 0.5;
                let clipped = text_height > composer_height + 0.5
                    || action_height > composer_height + 0.5;
                crate::bench::ui_fixture::publish_feedback_composer(
                    crate::bench::ui_fixture::FeedbackComposerSnapshot {
                        line_count: visible_line_count,
                        composer_height,
                        text_height,
                        action_height,
                        overlap_safe,
                        clipped,
                    },
                );
            })
            .id("chat-input-container")
            .font_family(echo_chat_render::text_run_intern::ui_font_family())
            .h(px(composer_height))
            .flex_shrink_0()
            .w_full()
            .overflow_hidden()
            .bg(colors::bg_elevated())
            .border_t_1()
            .border_color(if is_focused {
                colors::accent()
            } else {
                colors::border_subtle()
            })
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |_this, _event: &MouseDownEvent, window, cx| {
                    window.focus(&input_focus_for_click, cx);
                }),
            )
            .relative()
            .flex_col()
            .gap_0()
            .child(
                div()
                    .id("chat-input-shell")
                    .w_full()
                    .h(px((composer_height - 1.0).max(0.0)))
                    .flex_none()
                    .min_h_0()
                    .overflow_hidden()
                    .pr(px(actions_reserved_width + rail_width))
                    .flex()
                    .bg(colors::bg_elevated())
                    .child(
                        div()
                            .id("chat-input-field")
                            .role(Role::MultilineTextInput)
                            .aria_label(composer_a11y_label)
                            .aria_value(composer_a11y_text.clone())
                            .aria_placeholder(input_placeholder.clone())
                            .a11y_synthetic_children(move |builder| {
                                let targets = autocomplete_a11y.borrow();
                                let node = builder.parent_node();
                                node.set_has_popup(accesskit::HasPopup::Listbox);
                                node.set_auto_complete(accesskit::AutoComplete::List);
                                node.set_expanded(targets.popup_id.is_some());
                                if let Some(popup_id) = targets.popup_id {
                                    node.set_controls([popup_id]);
                                }
                                if let Some(active_id) = targets.active_id {
                                    node.set_active_descendant(active_id);
                                }
                                let mut run = accesskit::Node::new(Role::TextRun);
                                run.set_value(composer_a11y_text.to_string());
                                run.set_character_lengths(
                                    composer_a11y_text
                                        .chars()
                                        .map(|character| character.len_utf8() as u8)
                                        .collect::<Vec<_>>(),
                                );
                                let run_id = builder.synthetic_node_id(0);
                                builder.push_child(run_id, run);
                                let (anchor, focus) = composer_a11y_selection
                                    .unwrap_or((composer_a11y_cursor, composer_a11y_cursor));
                                builder.parent_node().set_text_selection(accesskit::TextSelection {
                                    anchor: accesskit::TextPosition {
                                        node: run_id,
                                        character_index: anchor,
                                    },
                                    focus: accesskit::TextPosition {
                                        node: run_id,
                                        character_index: focus,
                                    },
                                });
                            })
                            .flex_1()
                            .min_w_0()
                            .min_h(px(config::CHAT_INPUT_FIELD_HEIGHT_PX))
                            .overflow_hidden()
                            .pl(px(Spacing::SM))
                            .pr(px(Spacing::XS))
                            .py(px(Spacing::XXS))
                            .flex()
                            .flex_col()
                            .justify_center()
                            .gap_0()
                            .cursor_text()
                            .key_context(CHAT_INPUT_CONTEXT)
                            .track_focus(input_focus)
                            .on_mouse_down(MouseButton::Left, {
                                let input_focus = input_focus.clone();
                                let app = app.clone();
                                let channel = channel_for_input.clone();
                                let path = path_for_input.clone();
                                cx.listener(
                                    move |this, event: &MouseDownEvent, window, cx| {
                                        this.reset_inline_tab_completion(split_id);
                                        window.focus(&input_focus, cx);

                                        let changed = if event.click_count >= 3 {
                                            this.composer_pointer_selection = None;
                                            this.input_manager.select_all(split_id)
                                        } else if event.click_count == 2 {
                                            this.composer_pointer_selection = None;
                                            match this.composer_cursor_for_position(
                                                split_id,
                                                event.position,
                                                window,
                                            ) {
                                                Some(offset) => this
                                                    .input_manager
                                                    .select_word_at(split_id, offset),
                                                None => {
                                                    this.input_manager.select_all(split_id)
                                                }
                                            }
                                        } else if let Some(focus) = this
                                            .composer_cursor_for_position(
                                                split_id,
                                                event.position,
                                                window,
                                            )
                                        {
                                            this.begin_composer_pointer_selection(
                                                split_id,
                                                focus,
                                                event.modifiers.shift,
                                            )
                                        } else {
                                            false
                                        };

                                        if changed {
                                            Self::defer_autocomplete_refresh(
                                                &app,
                                                &channel,
                                                &path,
                                                this.input_manager
                                                    .draft(split_id)
                                                    .cloned(),
                                                cx,
                                            );
                                            cx.notify();
                                        }
                                    },
                                )
                            })
                            .on_mouse_down(MouseButton::Right, {
                                let input_focus = input_focus.clone();
                                cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                                    this.reset_inline_tab_completion(split_id);
                                    window.focus(&input_focus, cx);

                                    if let Some(offset) = this
                                        .composer_cursor_for_position(split_id, event.position, window)
                                    {
                                        let keep_selection = this
                                            .input_manager
                                            .selection_for_split(split_id)
                                            .is_some_and(|range| range.contains(&offset));
                                        if !keep_selection {
                                            this.input_manager
                                                .set_pointer_selection(split_id, offset, offset);
                                        }
                                    }

                                    let selection = this
                                        .input_manager
                                        .selection_for_split(split_id)
                                        .and_then(|range| {
                                            this.input_manager
                                                .copy_selection(split_id)
                                                .map(|text| ComposerSelectionSnapshot { range, text })
                                        });
                                    let has_text = this
                                        .input_manager
                                        .draft(split_id)
                                        .is_some_and(|text| !text.is_empty());
                                    backend::push_ui_action(backend::UiAction::ShowContextMenu(
                                        ContextMenuState::open_composer(
                                            event.position.x.into(),
                                            event.position.y.into(),
                                            split_id,
                                            selection,
                                            has_text,
                                            this.input_manager.can_undo(split_id),
                                            this.input_manager.can_redo(split_id),
                                        ),
                                    ));
                                    cx.stop_propagation();
                                    cx.notify();
                                })
                            })
                            .on_mouse_move({
                                cx.listener(
                                    move |this, event: &MouseMoveEvent, window, cx| {
                                        if !event.dragging() {
                                            return;
                                        }
                                        let Some(focus) = this.composer_cursor_for_position(
                                            split_id,
                                            event.position,
                                            window,
                                        ) else {
                                            return;
                                        };
                                        if this.update_composer_pointer_selection(split_id, focus) {
                                            cx.notify();
                                        }
                                    },
                                )
                            })
                            .on_mouse_up(MouseButton::Left, {
                                let target = target.clone();
                                cx.listener(move |this, _event: &MouseUpEvent, _window, cx| {
                                    this.end_composer_pointer_selection(&target, cx);
                                })
                            })
                            .on_mouse_up_out(MouseButton::Left, {
                                let target = target.clone();
                                cx.listener(move |this, _event: &MouseUpEvent, _window, cx| {
                                    this.end_composer_pointer_selection(&target, cx);
                                })
                            })
                            .on_action({
                                let channel = channel_for_input.clone();
                                let path = path_for_input.clone();
                                let app = app.clone();
                                cx.listener(move |this, action: &TabComplete, _window, cx| {
                                    this.handle_tab_complete_action(
                                        split_id,
                                        &channel,
                                        &path,
                                        &app,
                                        action.reverse,
                                        cx,
                                    );
                                    cx.stop_propagation();
                                })
                            })
                            .on_key_down({
                                let target = target.clone();
                                cx.listener(move |this, event: &KeyDownEvent, window, cx| {
                                    this.on_composer_key_down(event, &target, reply_visible, window, cx);
                                })
                            })
                            .when(show_context_row, |field| {
                                field.child(
                                    div()
                                        .id("chat-input-context-row")
                                        .w_full()
                                        .h(px(config::CHAT_INPUT_CONTEXT_ROW_HEIGHT_PX))
                                        .flex_shrink_0()
                                        .overflow_hidden()
                                        .line_height(px(config::CHAT_INPUT_CONTEXT_ROW_HEIGHT_PX))
                                        .flex()
                                        .items_center()
                                        .justify_between()
                                        .gap(px(Spacing::XS))
                                        .child(
                                            div()
                                                .flex_1()
                                                .min_w_0()
                                                .text_size(px(COMPOSER_SECONDARY_TEXT_PX))
                                                .text_color(if send_blocked
                                                    || eligibility_block.is_some()
                                                {
                                                    colors::warning()
                                                } else {
                                                    colors::text_muted()
                                                })
                                                .text_ellipsis()
                                                .whitespace_nowrap()
                                                .child(context_text.clone()),
                                        )
                                        .when(
                                            matches!(
                                                eligibility_block,
                                                Some(
                                                    backend::ui_actions::composer_send::EligibilityBlock::Banned
                                                        | backend::ui_actions::composer_send::EligibilityBlock::TimedOut(_)
                                                )
                                            ),
                                            |row| {
                                                let channel = channel.to_string();
                                                let app = app.clone();
                                                row.child(
                                                    div()
                                                        .id(("chat-input-try-again", split_id.as_u64()))
                                                        .flex_shrink_0()
                                                        .px(px(Spacing::XS))
                                                        .text_size(px(COMPOSER_SECONDARY_TEXT_PX))
                                                        .text_color(colors::text_muted())
                                                        .cursor_pointer()
                                                        .hover(|button| {
                                                            button.text_color(colors::text_primary())
                                                        })
                                                        .child("Try again")
                                                        .on_activate_without_tab_stop(
                                                            cx.listener(move |this, _, _window, cx| {
                                                                backend::irc::room_state::clear_current_send_restriction(&channel);
                                                                this.clear_eligibility_notice(split_id, &channel);
                                                                if let Some(composer) = this.eligibility_composers.get_mut(&split_id) {
                                                                    composer.last_block = None;
                                                                }
                                                                cx.notify();
                                                                app.update(cx, |app, cx| {
                                                                    if is_unified_input { cx.notify(); }
                                                                    else { app.notify_split_leaf(split_id, cx); }
                                                                });
                                                            }),
                                                        ),
                                                )
                                            },
                                        )
                                        .when(show_char_counter, |row| {
                                            row.child(
                                                div()
                                                    .flex_shrink_0()
                                                    .text_size(px(COMPOSER_SECONDARY_TEXT_PX))
                                                    .font_weight(FontWeight::MEDIUM)
                                                    .text_color(if input_char_count
                                                        > config::TWITCH_MESSAGE_CHAR_LIMIT
                                                    {
                                                        colors::error()
                                                    } else {
                                                        colors::text_muted()
                                                    })
                                                    .child(format!(
                                                        "{}/{}",
                                                        input_char_count,
                                                        config::TWITCH_MESSAGE_CHAR_LIMIT
                                                    )),
                                            )
                                        }),
                                )
                            })
                            .child(
                                div()
                                    .flex_1()
                                    .min_h_0()
                                    .w_full()
                                    .flex()
                                    .items_center()
                                    .overflow_hidden()
                                    .min_w_0()
                                    .gap(px(Spacing::XS))
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w_0()
                                            .overflow_hidden()
                                            .relative()
                                            .h(px(visible_line_count as f32 * COMPOSER_LINE_HEIGHT_PX))
                                            .child(Self::render_chat_text(
                                                cx.entity().clone(),
                                                split_id,
                                                &input_text,
                                                cursor_position,
                                                input_selection.clone(),
                                                ime_marked_range.clone(),
                                                is_placeholder,
                                                caret_active,
                                                caret_visible,
                                                send_blocked,
                                                &input_placeholder,
                                            ))
                                            .child(
                                                canvas(
                                                    move |bounds, _window, cx| {
                                                        if feedback_geometry_enabled {
                                                            let raw_height: f32 = bounds.size.height.into();
                                                            let visible_height = raw_height
                                                                .min((composer_height - 1.0).max(0.0));
                                                            let raw_y: f32 = bounds.origin.y.into();
                                                            let visible_y = if composer_overflows {
                                                                raw_y + raw_height - visible_height
                                                            } else {
                                                                raw_y
                                                            };
                                                            crate::bench::ui_fixture::publish_feedback_composer_pointer(
                                                                crate::bench::ui_fixture::FeedbackComposerPointerSnapshot {
                                                                    bounds: crate::bench::ui_fixture::FeedbackRect {
                                                                        x: bounds.origin.x.into(),
                                                                        y: visible_y,
                                                                        width: bounds.size.width.into(),
                                                                        height: visible_height,
                                                                    },
                                                                    text_len: feedback_pointer_text_len,
                                                                    cursor: cursor_position,
                                                                    selection_start: feedback_pointer_selection
                                                                        .as_ref()
                                                                        .map(|range| range.start),
                                                                    selection_end: feedback_pointer_selection
                                                                        .as_ref()
                                                                        .map(|range| range.end),
                                                                },
                                                            );
                                                        }
                                                        composer_bounds_entity.update(
                                                            cx,
                                                            |renderer, _| {
                                                                let changed = renderer
                                                                    .composer_text_bounds
                                                                    .get(&split_id)
                                                                    .is_none_or(|previous| {
                                                                        (previous.origin.x
                                                                            - bounds.origin.x)
                                                                            .abs()
                                                                            > px(0.5)
                                                                            || (previous.origin.y
                                                                                - bounds.origin.y)
                                                                                .abs()
                                                                                > px(0.5)
                                                                            || (previous.size.width
                                                                                - bounds.size.width)
                                                                                .abs()
                                                                                > px(0.5)
                                                                            || (previous.size.height
                                                                                - bounds.size.height)
                                                                                .abs()
                                                                                > px(0.5)
                                                                    });
                                                                if changed {
                                                                    renderer
                                                                        .composer_text_bounds
                                                                        .insert(split_id, bounds);
                                                                }
                                                            },
                                                        );
                                                    },
                                                    move |bounds, _: (), window, cx| {
                                                        if register_native_input
                                                            && let Some(handler) =
                                                                composer_input_handler.clone()
                                                        {
                                                            window.handle_input(
                                                                &composer_input_focus,
                                                                ElementInputHandler::new(
                                                                    bounds,
                                                                    handler,
                                                                ),
                                                                cx,
                                                            );
                                                        }
                                                    },
                                                )
                                                .absolute()
                                                .inset_0(),
                                            ),
                                    )
                                    .when(!draft_preview.items.is_empty(), |row| {
                                        row.child(Self::render_draft_preview_rail(
                                            draft_preview.as_ref(),
                                        ))
                                    }),
                            ),
                    ),
            )
            .child({
                let app_for_emote = app.clone();
                let app_for_send = app.clone();

                div()
                    .id("chat-input-actions-overlay")
                    .absolute()
                    .top_0()
                    .right_0()
                    .bottom_0()
                    .w(px(actions_reserved_width + rail_width))
                    .flex()
                    .items_center()
                    .justify_end()
                    .gap_0()
                    .border_l_1()
                    .border_color(colors::border_subtle())
                    .when_some(rail_plan, |actions, plan| {
                        actions.child(render_room_state_rail(
                            split_id.as_u64(),
                            plan,
                            &room_modes,
                            rail_target_label.as_deref(),
                            popover_open,
                            cx.listener(move |this, event: &ClickEvent, _window, cx| {
                                let dismissed_by_this_press =
                                    match (this.room_modes_dismiss_press.take(), event) {
                                        (Some(position), ClickEvent::Mouse(click)) => {
                                            click.down.position == position
                                        }
                                        _ => false,
                                    };
                                this.room_modes_popover = if dismissed_by_this_press
                                    || this.room_modes_popover == Some(split_id)
                                {
                                    None
                                } else {
                                    Some(split_id)
                                };
                                cx.notify();
                                cx.stop_propagation();
                            }),
                        ))
                    })
                    .child(
                        div()
                            .id("emote-picker-button")
                            .role(Role::Button)
                            .aria_label("Open emote picker")
                            .w(px(44.0))
                            .h_full()
                            .cursor_pointer()
                            .flex()
                            .items_center()
                            .justify_center()
                            .hover(|s| s.bg(colors::bg_hover()))
                            .on_accessible_activate(
                                {
                                    let app = app_for_emote.clone();
                                    let channel = channel_for_input.clone();
                                    move |_event, window, cx| {
                                        app.update(cx, |app_instance, cx| {
                                            if let Some(path) = app_instance.activate_composer_split(split_id, &channel, cx) {
                                                app_instance.toggle_emote_picker(&path, window, cx);
                                            }
                                        });
                                    }
                                },
                            )
                            .child(
                                div()
                                    .w(px(16.0))
                                    .h(px(16.0))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .pt(px(Spacing::HAIRLINE))
                                    .child(
                                        svg()
                                            .path("icons/smile.svg")
                                            .size(px(16.0))
                                            .text_color(if is_focused {
                                                colors::accent_hover()
                                            } else {
                                                colors::text_secondary()
                                            }),
                                    ),
                            ),
                    )
                    .when(show_send_button, |s| {
                        s.child(
                            div()
                                .id("send-button")
                                .role(Role::Button)
                                .aria_label("Send message")
                                .w(px(64.0))
                                .h_full()
                                .px(px(Spacing::XS_PLUS))
                                .bg(colors::accent_transparent(0.16))
                                .cursor_pointer()
                                .flex()
                                .items_center()
                                .justify_center()
                                .hover(|s| s.bg(colors::accent_transparent(0.22)))
                                .on_accessible_activate({
                                    let app = app_for_send.clone();
                                    let channel = channel_for_input.clone();
                                    move |_event, _window, cx| {
                                        app.update(cx, |app_instance, cx| {
                                            app_instance.send_message_for_split(split_id, &channel, cx);
                                        });
                                    }
                                })
                                .child(
                                    div()
                                        .text_size(px(COMPOSER_ACTION_TEXT_PX))
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .text_color(colors::accent_hover())
                                        .child("Send"),
                                ),
                        )
                    })
            });

        let popover = popover_open.then(|| {
            render_room_state_popover(
                &room_modes,
                cx.listener(move |this, event: &MouseDownEvent, _window, cx| {
                    if this.room_modes_popover == Some(split_id) {
                        this.room_modes_popover = None;
                        this.room_modes_dismiss_press = Some(event.position);
                        cx.notify();
                    }
                }),
            )
        });

        self.composer_draft_scratch = input_text;
        RenderedComposer {
            element: input.into_any_element(),
            has_animated_draft_previews,
            room_modes_popover: popover,
        }
    }

    /// Key handling for a split's composer: send, history, completion,
    /// clipboard and caret movement. `reply_visible` is whether the reply
    /// context row is showing, which Escape dismisses first.
    fn on_composer_key_down(
        &mut self,
        event: &KeyDownEvent,
        target: &ComposerTarget,
        reply_visible: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let this = self;
        let ComposerTarget {
            app,
            channel,
            path,
            split_id,
        } = target;
        let split_id = *split_id;
        {
            let overlays = app.read(cx).overlay_manager.read(cx);
            if overlays.context_menu.read(cx).is_open()
                || overlays.quick_switcher.read(cx).is_open
                || overlays.confirmation_modal.read(cx).state.is_open
            {
                return;
            }
        }

        let is_active = {
            let app = app.read(cx);
            let layout = app.split_layout.read(cx);
            layout.get_active_split_path() == path.as_ref()
        };

        if !is_active {
            return;
        }

        // A pending multiline-paste prompt owns Escape unless IME text is marked.
        if event.keystroke.key == "escape"
            && (this.ime_marked_ranges.contains_key(&split_id)
                || !this.multiline_pastes.contains_key(&split_id))
        {
            if event.is_held {
                cx.stop_propagation();
                return;
            }
            if this.handle_composer_escape(split_id, reply_visible, app, cx) {
                cx.stop_propagation();
                return;
            }
        }

        if event.keystroke.key == "enter" && this.ime_marked_ranges.contains_key(&split_id) {
            cx.stop_propagation();
            return;
        }

        let channel = channel.clone();
        let path = path.clone();

        let (
            autocomplete_open,
            ac_selected_idx,
            ac_suggestion_count,
            ac_suggestion_revision,
            ac_type,
            ac_query_chars,
            ac_exact_match,
        ) = {
            let app_data = app.read(cx);
            let ac = app_data.overlay_manager.read(cx).autocomplete.read(cx);
            (
                ac.state.is_open,
                ac.state.selected_index,
                ac.suggestions.len(),
                ac.suggestion_revision(),
                ac.state.ac_type,
                ac.state.query.chars().count(),
                event.keystroke.key == "enter"
                    && ac
                        .selection_at(ac.suggestion_revision(), ac.state.selected_index)
                        .is_some_and(|selection| {
                            completion_matches_fragment(
                                &selection.context.expected_fragment,
                                &selection.text,
                            )
                        }),
            )
        };

        this.reset_inline_tab_completion(split_id);

        let alt = event.keystroke.modifiers.alt;
        let shortcut_modifier =
            (event.keystroke.modifiers.control || event.keystroke.modifiers.platform) && !alt;
        let plain_or_shift =
            !alt && !event.keystroke.modifiers.control && !event.keystroke.modifiers.platform;
        if let Some(choice) = this.multiline_pastes.get_mut(&split_id) {
            match event.keystroke.key.as_str() {
                "up" | "down" if plain_or_shift => {
                    cx.stop_propagation();
                    let step = if event.keystroke.key == "up" { 2 } else { 1 };
                    choice.selected_index = (choice.selected_index + step) % 3;
                    cx.notify();
                    Self::defer_unified_input_notify(app, cx);
                    return;
                }
                "enter" | "escape" => {
                    cx.stop_propagation();
                    if event.is_held {
                        return;
                    }
                    let id = choice.id;
                    let action = if event.keystroke.key == "escape" {
                        MultilinePasteAction::Cancel
                    } else {
                        match choice.selected_index {
                            1 => MultilinePasteAction::SendLines,
                            2 => MultilinePasteAction::Cancel,
                            _ => MultilinePasteAction::OneLine,
                        }
                    };
                    let app = app.clone();
                    cx.defer(move |cx| {
                        app.update(cx, |app, cx| {
                            app.resolve_multiline_paste(split_id, Some(id), action, cx)
                        })
                    });
                    return;
                }
                _ => {}
            }
        }
        let paste_shortcut = event.keystroke.key.eq_ignore_ascii_case("v") && shortcut_modifier;
        if paste_shortcut {
            cx.stop_propagation();
            let uploader_config = backend::settings::get_image_uploader_config();
            let pasted = match Self::read_clipboard_paste(cx) {
                Some(ClipboardPaste::Text(text)) => text,
                Some(ClipboardPaste::Files(paths)) => {
                    let source = backend::image_uploader::ImageFileSource::Pasted;
                    // Only an all-image selection uploads;
                    // videos and mixed copies paste paths.
                    let all_images = paths.iter().all(|path| {
                        backend::image_uploader::mime_type_for_image_path(path)
                            .is_some_and(|mime| mime.starts_with("image/"))
                    });
                    if all_images && source.upload_enabled(&uploader_config) {
                        app.update(cx, |app_instance, cx| {
                            app_instance.confirm_or_upload_image_files(
                                channel.to_string(),
                                split_id,
                                paths,
                                source,
                                cx,
                            );
                        });
                        return;
                    }
                    paths
                        .iter()
                        .map(|path| path.display().to_string())
                        .collect::<Vec<_>>()
                        .join(" ")
                }
                None => String::new(),
            };
            let prefer_image = pasted.is_empty() || event.keystroke.modifiers.shift;
            if prefer_image && uploader_config.enabled && uploader_config.paste_upload_enabled {
                #[cfg(target_os = "linux")]
                let read_image = {
                    let image = Self::clipboard_image(cx);
                    cx.background_spawn(async move { Self::decode_clipboard_image(image) })
                };
                #[cfg(not(target_os = "linux"))]
                let read_image =
                    cx.background_spawn(async { Self::read_clipboard_image_for_upload() });
                let app = app.clone();
                let channel = channel.clone();
                let path = path.clone();
                cx.spawn(async move |this, cx| match read_image.await {
                    Ok(Some((width, height, rgba))) => {
                        let source_bytes = rgba.len() as u64;
                        app.update(cx, |app_instance, cx| {
                            app_instance.confirm_or_upload_pasted_image(
                                channel.to_string(),
                                split_id,
                                crate::app::PastedImage {
                                    width,
                                    height,
                                    rgba,
                                    source_bytes,
                                },
                                cx,
                            );
                        });
                    }
                    result => {
                        if let Err(err) = result {
                            tracing::debug!("Clipboard image upload paste skipped: {err}");
                        }
                        if !pasted.is_empty() {
                            let _ = this.update(cx, |this, cx| {
                                this.insert_pasted_text(
                                    split_id, &pasted, &app, &channel, &path, cx,
                                );
                            });
                        }
                    }
                })
                .detach();
                return;
            }

            if !pasted.is_empty() {
                this.insert_pasted_text(split_id, &pasted, app, &channel, &path, cx);
            }
            return;
        }

        let composer_shortcut = shortcut_modifier
            && (!event.keystroke.modifiers.shift || event.keystroke.key.eq_ignore_ascii_case("z"));
        if composer_shortcut {
            let shortcut_key = event.keystroke.key.to_ascii_lowercase();
            let mut should_refresh_autocomplete = false;
            let mut reset_history_navigation = false;

            match shortcut_key.as_str() {
                "a" => {
                    should_refresh_autocomplete = this.input_manager.select_all(split_id);
                }
                "c" => {
                    if let Some(selected) = this.input_manager.copy_selection(split_id) {
                        cx.write_to_clipboard(ClipboardItem::new_string(selected));
                        cx.stop_propagation();
                    }
                    return;
                }
                "x" => {
                    if let Some(selected) = this.input_manager.cut_selection(split_id) {
                        cx.write_to_clipboard(ClipboardItem::new_string(selected));
                        should_refresh_autocomplete = true;
                        reset_history_navigation = true;
                    }
                }
                "z" => {
                    should_refresh_autocomplete = if event.keystroke.modifiers.shift {
                        this.input_manager.redo(split_id)
                    } else {
                        this.input_manager.undo(split_id)
                    };
                    reset_history_navigation = should_refresh_autocomplete;
                }
                "y" => {
                    should_refresh_autocomplete = this.input_manager.redo(split_id);
                    reset_history_navigation = should_refresh_autocomplete;
                }
                _ => {}
            }

            if should_refresh_autocomplete {
                if reset_history_navigation {
                    this.ime_marked_ranges.remove(&split_id);
                }
                this.reset_composer_affinity(split_id);
                if reset_history_navigation {
                    this.input_manager.end_history_navigation(split_id);
                }
                Self::defer_autocomplete_refresh(
                    app,
                    &channel,
                    &path,
                    this.input_manager.draft(split_id).cloned(),
                    cx,
                );
                cx.notify();
            }

            if matches!(shortcut_key.as_str(), "a" | "c" | "x" | "z" | "y") {
                cx.stop_propagation();
                return;
            }
        }

        if event.keystroke.key == "enter" && shortcut_modifier {
            cx.stop_propagation();
            if event.is_held {
                return;
            }
            let app = app.clone();
            let channel = channel.clone();
            let path = path.clone();
            let preserved_text = this.input_manager.text_for_split(split_id).to_string();
            let preserved_cursor = this.input_manager.cursor_for_split(split_id);
            cx.defer(move |cx| {
                app.update(cx, |app_instance, cx| {
                    app_instance.send_message(cx);

                    let should_restore = !preserved_text.trim().is_empty()
                        && !app_instance
                            .chat_input_renderer
                            .read(cx)
                            .input_manager
                            .has_draft(split_id);
                    if should_restore {
                        app_instance.chat_input_renderer.update(cx, |renderer, _| {
                            renderer
                                .input_manager
                                .set_draft_text(split_id, preserved_text.clone());
                            renderer
                                .input_manager
                                .set_cursor_for_split(split_id, preserved_cursor);
                            renderer.reset_composer_affinity(split_id);
                            renderer.input_manager.drop_history_stash(split_id);
                        });
                        app_instance.update_autocomplete(
                            &channel,
                            &path,
                            Some(&preserved_text),
                            cx,
                        );
                        cx.notify();
                    }
                });
            });
            return;
        }

        if event.keystroke.key == "enter" && !event.keystroke.modifiers.shift && !alt {
            cx.stop_propagation();
            if event.is_held {
                return;
            }
            let disposition = if ac_exact_match {
                EnterKeyDisposition::SendInput
            } else {
                enter_key_disposition(
                    autocomplete_open,
                    ac_suggestion_count,
                    ac_type,
                    ac_query_chars,
                )
            };
            let app = app.clone();
            cx.defer(move |cx| {
                app.update(cx, |app_instance, cx| {
                    let selection_accepted =
                        match disposition {
                            EnterKeyDisposition::AcceptSelection => app_instance
                                .accept_autocomplete(ac_suggestion_revision, ac_selected_idx, cx),
                            EnterKeyDisposition::SendInput => false,
                        };
                    if selection_accepted {
                        app_instance
                            .chat_input_renderer
                            .update(cx, |renderer, _| renderer.reset_composer_affinity(split_id));
                    }
                    if disposition.should_send_after_accept(selection_accepted) {
                        if autocomplete_open {
                            app_instance
                                .autocomplete(cx)
                                .update(cx, |ac, _| ac.state.close());
                        }
                        app_instance.send_message(cx);
                    }
                });
            });
            return;
        }

        let alt_word_delete = alt
            && !event.keystroke.modifiers.control
            && !event.keystroke.modifiers.platform
            && matches!(event.keystroke.key.as_str(), "backspace" | "delete")
            && backend::hotkeys::match_key_event(
                event,
                backend::hotkeys::HotkeyMatchContext {
                    allow_global: true,
                    has_active_split: true,
                    ..Default::default()
                },
                true,
            )
            .is_none();

        if event.keystroke.key == "backspace" && (!alt || alt_word_delete) {
            cx.stop_propagation();
            if this
                .input_manager
                .delete_backward(split_id, shortcut_modifier || alt_word_delete)
            {
                this.reset_composer_affinity(split_id);
            }
            Self::defer_autocomplete_refresh(
                app,
                &channel,
                &path,
                this.input_manager.draft(split_id).cloned(),
                cx,
            );
            cx.notify();
            return;
        }

        if event.keystroke.key == "delete" && (!alt || alt_word_delete) {
            cx.stop_propagation();
            if this
                .input_manager
                .delete_forward(split_id, shortcut_modifier || alt_word_delete)
            {
                this.reset_composer_affinity(split_id);
            }
            Self::defer_autocomplete_refresh(
                app,
                &channel,
                &path,
                this.input_manager.draft(split_id).cloned(),
                cx,
            );
            cx.notify();
            return;
        }

        if event.keystroke.key == "left" && !alt {
            cx.stop_propagation();
            if this.input_manager.move_cursor_left(
                split_id,
                shortcut_modifier,
                event.keystroke.modifiers.shift,
            ) {
                this.reset_composer_affinity(split_id);
                Self::defer_autocomplete_refresh(
                    app,
                    &channel,
                    &path,
                    this.input_manager.draft(split_id).cloned(),
                    cx,
                );
                cx.notify();
            }
            return;
        }

        if event.keystroke.key == "right" && !alt {
            cx.stop_propagation();
            if this.input_manager.move_cursor_right(
                split_id,
                shortcut_modifier,
                event.keystroke.modifiers.shift,
            ) {
                this.reset_composer_affinity(split_id);
                Self::defer_autocomplete_refresh(
                    app,
                    &channel,
                    &path,
                    this.input_manager.draft(split_id).cloned(),
                    cx,
                );
                cx.notify();
            }
            return;
        }

        if matches!(event.keystroke.key.as_str(), "home" | "end") && !alt {
            cx.stop_propagation();
            let end = event.keystroke.key == "end";
            if this.move_composer_line_edge(
                split_id,
                end,
                shortcut_modifier,
                event.keystroke.modifiers.shift,
                window,
            ) {
                Self::defer_autocomplete_refresh(
                    app,
                    &channel,
                    &path,
                    this.input_manager.draft(split_id).cloned(),
                    cx,
                );
                cx.notify();
            }
            return;
        }

        if event.keystroke.key == "up" && plain_or_shift {
            cx.stop_propagation();
            if autocomplete_open && ac_suggestion_count > 0 && !event.keystroke.modifiers.shift {
                let new_idx = if ac_selected_idx == 0 {
                    ac_suggestion_count - 1
                } else {
                    ac_selected_idx - 1
                };
                let app = app.clone();
                cx.defer(move |cx| {
                    app.update(cx, |app_instance, cx| {
                        app_instance.overlay_manager.update(cx, |om, cx| {
                            om.autocomplete.update(cx, |ac, _| {
                                ac.select_index_from_keyboard(new_idx);
                            });
                        });
                    });
                });
                return;
            }
            let shift = event.keystroke.modifiers.shift;
            let moved = this.move_composer_vertical(split_id, -1, shift, window);
            let recalled = moved == Some(false)
                && !shift
                && this.input_manager.history_up(&channel, split_id).is_some();
            if recalled {
                this.reset_composer_affinity(split_id);
            }
            if moved == Some(true) || recalled {
                Self::defer_autocomplete_refresh(
                    app,
                    &channel,
                    &path,
                    this.input_manager.draft(split_id).cloned(),
                    cx,
                );
                cx.notify();
            }
            return;
        }

        if event.keystroke.key == "down" && plain_or_shift {
            cx.stop_propagation();
            if autocomplete_open && ac_suggestion_count > 0 && !event.keystroke.modifiers.shift {
                let new_idx = (ac_selected_idx + 1) % ac_suggestion_count;
                let app = app.clone();
                cx.defer(move |cx| {
                    app.update(cx, |app_instance, cx| {
                        app_instance.overlay_manager.update(cx, |om, cx| {
                            om.autocomplete.update(cx, |ac, _| {
                                ac.select_index_from_keyboard(new_idx);
                            });
                        });
                    });
                });
                return;
            }
            let shift = event.keystroke.modifiers.shift;
            let moved = this.move_composer_vertical(split_id, 1, shift, window);
            let recalled = moved == Some(false)
                && !shift
                && this
                    .input_manager
                    .history_down(&channel, split_id)
                    .is_some();
            if recalled {
                this.reset_composer_affinity(split_id);
            }
            if moved == Some(true) || recalled {
                Self::defer_autocomplete_refresh(
                    app,
                    &channel,
                    &path,
                    this.input_manager.draft(split_id).cloned(),
                    cx,
                );
                cx.notify();
            }
        }
    }

    /// The clipboard image a paste would upload. On Linux it must be read
    /// through GPUI on the UI thread (see `utils::clipboard`).
    #[cfg(target_os = "linux")]
    fn clipboard_image(cx: &App) -> Option<Image> {
        cx.read_from_clipboard()?
            .entries()
            .iter()
            .find_map(|entry| match entry {
                ClipboardEntry::Image(image) => Some(image.clone()),
                _ => None,
            })
    }

    /// Decode a GPUI clipboard image into the RGBA rows arboard yields.
    #[cfg(target_os = "linux")]
    fn decode_clipboard_image(
        image: Option<Image>,
    ) -> Result<Option<(usize, usize, Vec<u8>)>, String> {
        let Some(image) = image else {
            return Ok(None);
        };
        let decoded = image::load_from_memory(&image.bytes)
            .map_err(|err| format!("Failed to decode clipboard image: {err}"))?
            .into_rgba8();
        let (width, height) = decoded.dimensions();
        Ok(Some((width as usize, height as usize, decoded.into_raw())))
    }

    #[cfg(not(target_os = "linux"))]
    fn read_clipboard_image_for_upload() -> Result<Option<(usize, usize, Vec<u8>)>, String> {
        let mut clipboard =
            arboard::Clipboard::new().map_err(|e| format!("Failed to open clipboard: {e}"))?;
        match clipboard.get_image() {
            Ok(image) => {
                let expected_len = image
                    .width
                    .checked_mul(image.height)
                    .and_then(|pixels| pixels.checked_mul(4))
                    .ok_or_else(|| "Clipboard image dimensions are too large".to_string())?;
                if image.bytes.len() != expected_len {
                    return Err(format!(
                        "Clipboard image buffer has {} bytes, expected {}",
                        image.bytes.len(),
                        expected_len
                    ));
                }
                Ok(Some((image.width, image.height, image.bytes.into_owned())))
            }
            Err(arboard::Error::ContentNotAvailable) => Ok(None),
            Err(err) => Err(format!("Failed to read clipboard image: {err}")),
        }
    }

    /// Plain text on the clipboard, or the copied files when it has no text,
    /// without reading any image format.
    fn read_clipboard_paste(cx: &App) -> Option<ClipboardPaste> {
        #[cfg(target_os = "linux")]
        {
            let text = crate::utils::read_text(cx)?;
            (!text.is_empty()).then_some(ClipboardPaste::Text(text))
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = cx;
            let mut clipboard = arboard::Clipboard::new().ok()?;
            match clipboard.get_text() {
                Ok(text) if !text.is_empty() => Some(ClipboardPaste::Text(text)),
                _ => {
                    let paths = clipboard.get().file_list().ok()?;
                    (!paths.is_empty()).then_some(ClipboardPaste::Files(paths))
                }
            }
        }
    }

    fn insert_pasted_text(
        &mut self,
        split_id: SplitId,
        pasted: &str,
        app: &Entity<crate::app::EchoApp>,
        channel: &Arc<str>,
        path: &Arc<[usize]>,
        cx: &mut Context<Self>,
    ) {
        if self.offer_multiline_paste(split_id, channel, pasted, cx) {
            Self::defer_unified_input_notify(app, cx);
            return;
        }
        self.insert_composer_paste(split_id, channel, pasted, cx);
        Self::defer_autocomplete_refresh(
            app,
            channel,
            path,
            self.input_manager.draft(split_id).cloned(),
            cx,
        );
        cx.notify();
    }

    fn paste_prefix(text: &str, limit: usize) -> (&str, bool) {
        match text.char_indices().nth(limit) {
            Some((end, _)) => (&text[..end], true),
            None => (text, false),
        }
    }

    fn paste_limit_notice() -> String {
        format!("Paste limited to {MAX_COMPOSER_PASTE_CHARS} characters; extra text was not pasted")
    }

    pub(crate) fn insert_composer_paste(
        &mut self,
        split_id: SplitId,
        channel: &str,
        pasted: &str,
        cx: &mut Context<Self>,
    ) -> bool {
        let text = self.input_manager.text_for_split(split_id);
        let retained_chars =
            if let Some(selection) = self.input_manager.selection_for_split(split_id) {
                text[..selection.start].chars().count() + text[selection.end..].chars().count()
            } else {
                text.chars().count()
            };
        let available = MAX_COMPOSER_PASTE_CHARS.saturating_sub(retained_chars);
        let (pasted, truncated) = Self::paste_prefix(pasted, available);
        let pasted = crate::components::ui::input::normalize_single_line_text(pasted);
        if truncated {
            self.set_send_notice(split_id, channel, Self::paste_limit_notice(), cx);
        }
        let changed = self.input_manager.insert_text_at_cursor(split_id, &pasted);
        if changed {
            self.reset_composer_affinity(split_id);
            self.input_manager.end_history_navigation(split_id);
            cx.notify();
        }
        changed
    }

    /// Finish a pointer drag selection in the composer and refresh
    /// autocomplete for the caret it left behind.
    fn end_composer_pointer_selection(&mut self, target: &ComposerTarget, cx: &mut Context<Self>) {
        if self.finish_composer_pointer_selection(target.split_id) {
            Self::defer_autocomplete_refresh(
                &target.app,
                &target.channel,
                &target.path,
                self.input_manager.draft(target.split_id).cloned(),
                cx,
            );
        }
    }

    fn defer_autocomplete_refresh(
        app: &Entity<crate::app::EchoApp>,
        channel: &Arc<str>,
        path: &Arc<[usize]>,
        input_text: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let app = app.clone();
        let channel = Arc::clone(channel);
        let path = Arc::clone(path);
        cx.defer(move |cx| {
            app.update(cx, |app, cx| {
                app.update_autocomplete(&channel, &path, input_text.as_deref(), cx);
            });
        });
    }

    fn defer_unified_input_notify(app: &Entity<crate::app::EchoApp>, cx: &mut Context<Self>) {
        let app = app.clone();
        cx.defer(move |cx| {
            app.update(cx, |app, cx| {
                if app.split_layout.read(cx).is_unified_input_mode() {
                    cx.notify();
                }
            });
        });
    }

    /// Resolve a window-space pointer position to the nearest UTF-8 byte
    /// boundary in the custom composer. This deliberately uses GPUI's wrapped
    /// glyph layout instead of average-character math, so proportional fonts,
    /// emoji, hard newlines, and narrow split wrapping share the same geometry
    /// contract as multiline height calculation.
    fn composer_font(&self, split_id: SplitId) -> Font {
        self.composer_layouts
            .get(&split_id)
            .map(|entry| entry.font.clone())
            .unwrap_or_else(|| font(echo_chat_render::text_run_intern::ui_font_family()))
    }

    fn composer_layout(
        &mut self,
        split_id: SplitId,
        window: &Window,
    ) -> Option<Arc<ComposerTextLayout>> {
        let width = self.composer_text_bounds.get(&split_id)?.size.width;
        let text = self.input_manager.text_for_split(split_id);
        if let Some(entry) = self.composer_layouts.get(&split_id)
            && entry.input_text == text
            && entry.width == width
        {
            return Some(Arc::clone(&entry.layout));
        }
        let font = self.composer_font(split_id);
        let lines = Self::shape_composer_text(text, font.clone(), width.as_f32(), window)?;
        let layout = Arc::new(ComposerTextLayout::new(lines));
        self.composer_layouts.insert(
            split_id,
            ComposerLayoutCacheEntry {
                input_text: text.to_string(),
                font,
                width,
                layout: Arc::clone(&layout),
            },
        );
        Some(layout)
    }

    pub(crate) fn reset_composer_affinity(&mut self, split_id: SplitId) {
        if let Some(viewport) = self.composer_viewports.get_mut(&split_id) {
            viewport.preferred_x = None;
            viewport.caret_row = None;
        }
    }

    fn move_composer_vertical(
        &mut self,
        split_id: SplitId,
        direction: isize,
        extend: bool,
        window: &Window,
    ) -> Option<bool> {
        let layout = self.composer_layout(split_id, window)?;
        let cursor = self.input_manager.cursor_for_split(split_id);
        let viewport = self.composer_viewports.entry(split_id).or_default();
        let row = layout.row_for_cursor(cursor, viewport.caret_row)?;
        let target_row = row
            .saturating_add_signed(direction)
            .min(layout.rows.len().saturating_sub(1));
        if target_row == row && !extend {
            return Some(false);
        }
        let position = layout.cursor_position(cursor, viewport.caret_row)?;
        let x = *viewport.preferred_x.get_or_insert(position.x);
        let next = if target_row == row {
            if direction < 0 {
                0
            } else {
                self.input_manager.text_for_split(split_id).len()
            }
        } else {
            layout.index_at(target_row, x)
        };
        viewport.caret_row = Some((next, target_row));
        self.input_manager.move_cursor_to(split_id, next, extend);
        Some(true)
    }

    fn move_composer_line_edge(
        &mut self,
        split_id: SplitId,
        end: bool,
        whole_draft: bool,
        extend: bool,
        window: &Window,
    ) -> bool {
        let cursor = self.input_manager.cursor_for_split(split_id);
        let hint = self
            .composer_viewports
            .get(&split_id)
            .and_then(|viewport| viewport.caret_row);
        let Some(layout) = self.composer_layout(split_id, window) else {
            return false;
        };
        let Some(current_row) = layout.row_for_cursor(cursor, hint) else {
            return false;
        };
        let row = if whole_draft {
            if end { layout.rows.len() - 1 } else { 0 }
        } else {
            current_row
        };
        let next = if end {
            layout.rows[row].range.end
        } else {
            layout.rows[row].range.start
        };
        let viewport = self.composer_viewports.entry(split_id).or_default();
        viewport.preferred_x = None;
        viewport.caret_row = Some((next, row));
        self.input_manager.move_cursor_to(split_id, next, extend);
        true
    }

    fn composer_cursor_for_position(
        &mut self,
        split_id: SplitId,
        position: Point<Pixels>,
        window: &Window,
    ) -> Option<usize> {
        let bounds = self.composer_text_bounds.get(&split_id).copied()?;
        let layout = self.composer_layout(split_id, window)?;
        if layout.rows.is_empty() {
            return Some(0);
        }
        let viewport = self.composer_viewports.entry(split_id).or_default();
        let inset = Self::composer_top_inset(&layout.lines, bounds.size.height.as_f32());
        let y = (position.y - bounds.top()).as_f32() - inset + viewport.offset_y;
        let row = ((y.max(0.0) / COMPOSER_LINE_HEIGHT_PX) as usize).min(layout.rows.len() - 1);
        let next = layout.index_at(row, position.x - bounds.left());
        viewport.preferred_x = None;
        viewport.caret_row = Some((next, row));
        Some(next)
    }

    fn begin_composer_pointer_selection(
        &mut self,
        split_id: SplitId,
        focus: usize,
        extend_existing: bool,
    ) -> bool {
        let anchor = if extend_existing {
            let cursor = self.input_manager.cursor_for_split(split_id);
            self.input_manager
                .selection_for_split(split_id)
                .map(|selection| {
                    if cursor == selection.start {
                        selection.end
                    } else {
                        selection.start
                    }
                })
                .unwrap_or(cursor)
        } else {
            focus
        };
        self.composer_pointer_selection = Some(ComposerPointerSelection { split_id, anchor });
        self.input_manager
            .set_pointer_selection(split_id, anchor, focus)
    }

    fn update_composer_pointer_selection(&mut self, split_id: SplitId, focus: usize) -> bool {
        let Some(selection) = self
            .composer_pointer_selection
            .filter(|selection| selection.split_id == split_id)
        else {
            return false;
        };
        self.input_manager
            .set_pointer_selection(split_id, selection.anchor, focus)
    }

    fn finish_composer_pointer_selection(&mut self, split_id: SplitId) -> bool {
        if self
            .composer_pointer_selection
            .is_some_and(|selection| selection.split_id == split_id)
        {
            self.composer_pointer_selection = None;
            true
        } else {
            false
        }
    }

    /// Wrapped line count and grapheme count for the current draft, recomputed
    /// only when the draft, the editable width, or the resolved font changes.
    fn composer_metrics(
        &mut self,
        split_id: SplitId,
        input_text: &str,
        available_width: f32,
        window: &Window,
    ) -> (usize, usize) {
        let font = self.composer_font(split_id);
        let available_width_bits = available_width.to_bits();
        if let Some(entry) = self.composer_metrics_cache.get(&split_id)
            && entry.available_width_bits == available_width_bits
            && entry.font == font
            && entry.input_text == input_text
        {
            return (entry.line_count, entry.grapheme_count);
        }

        let line_count = Self::composer_line_count(input_text, available_width, &font, window);
        let grapheme_count = input_text.graphemes(true).count();
        match self.composer_metrics_cache.entry(split_id) {
            std::collections::hash_map::Entry::Occupied(mut occupied) => {
                let entry = occupied.get_mut();
                if entry.input_text != input_text {
                    entry.input_text.clear();
                    entry.input_text.push_str(input_text);
                }
                entry.available_width_bits = available_width_bits;
                entry.font = font;
                entry.line_count = line_count;
                entry.grapheme_count = grapheme_count;
            }
            std::collections::hash_map::Entry::Vacant(vacant) => {
                vacant.insert(ComposerMetricsCacheEntry {
                    input_text: input_text.to_string(),
                    available_width_bits,
                    font,
                    line_count,
                    grapheme_count,
                });
            }
        }
        (line_count, grapheme_count)
    }

    fn shape_composer_text(
        input_text: &str,
        font: Font,
        width: f32,
        window: &Window,
    ) -> Option<smallvec::SmallVec<[WrappedLine; 1]>> {
        let run = TextRun {
            len: input_text.len(),
            font,
            color: colors::text_primary(),
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        window
            .text_system()
            .shape_text(
                SharedString::from(input_text.to_string()),
                px(COMPOSER_PRIMARY_TEXT_PX),
                std::slice::from_ref(&run),
                Some(px(width.max(1.0))),
                None,
            )
            .ok()
    }

    fn composer_top_inset(lines: &[WrappedLine], bounds_height: f32) -> f32 {
        let total_height = lines
            .iter()
            .map(|line| (line.wrap_boundaries().len() + 1) as f32 * COMPOSER_LINE_HEIGHT_PX)
            .sum::<f32>();
        ((bounds_height - total_height.min(bounds_height)) / 2.0).max(0.0)
    }

    fn composer_line_count(
        input_text: &str,
        available_width: f32,
        font: &Font,
        window: &Window,
    ) -> usize {
        if input_text.is_empty() {
            return 1;
        }

        let hard_line_count = input_text.split('\n').count().max(1);
        let measured_line_count =
            Self::shape_composer_text(input_text, font.clone(), available_width, window)
                .map(|lines| {
                    lines
                        .iter()
                        .map(|line| line.wrap_boundaries().len() + 1)
                        .sum::<usize>()
                })
                .unwrap_or(hard_line_count);
        measured_line_count.max(1)
    }

    /// Composer envelope for a measured draft.
    ///
    /// The base is content-dependent: a plain single-line draft only needs one
    /// 18px line plus 2px padding per side and the 1px border, while the
    /// context row adds its own explicit 14px line box. Extra wrapped lines
    /// grow the surface one line height each, up to the bounded viewport.
    fn composer_surface_height(visible_line_count: usize, has_context_row: bool) -> f32 {
        let extra_lines = visible_line_count
            .clamp(1, COMPOSER_MAX_VISIBLE_LINES)
            .saturating_sub(1);
        let base = if has_context_row {
            config::CHAT_INPUT_CONTEXT_HEIGHT_PX
        } else {
            config::CHAT_INPUT_HEIGHT_PX
        };
        base + extra_lines as f32 * COMPOSER_LINE_HEIGHT_PX
    }

    fn draft_preview_width(preview: &DraftPreview) -> f32 {
        if preview.items.is_empty() {
            return 0.0;
        }
        let item_width: f32 = preview.items.iter().map(|item| item.width_px).sum();
        let has_overflow = preview.overflow_count > 0;
        let child_count = preview.items.len() + usize::from(has_overflow);
        let gaps = child_count.saturating_sub(1) as f32 * DRAFT_PREVIEW_GAP_PX;
        let overflow_width = if has_overflow {
            DRAFT_PREVIEW_OVERFLOW_LABEL_WIDTH_PX
        } else {
            0.0
        };
        (DRAFT_PREVIEW_RAIL_BORDER_PX
            + DRAFT_PREVIEW_RAIL_PADDING_PX
            + item_width
            + gaps
            + overflow_width)
            .min(DRAFT_PREVIEW_RAIL_WIDTH_PX)
    }

    fn truncate_preview_text(preview: &str, max_chars: usize) -> String {
        if preview.contains("http://") || preview.contains("https://") {
            return preview.to_string();
        }
        crate::utils::truncate_text(preview, max_chars)
    }

    fn send_block_status_text(block: &backend::irc::SendBlock) -> &'static str {
        use backend::irc::SendBlock;
        match block {
            SendBlock::Anonymous => "Anonymous chat active - switch back to your account to send",
            SendBlock::NeedsReauth | SendBlock::NoChatToken { .. } => {
                "Re-authentication required before sending"
            }
            SendBlock::NoAccount => "Log in to Twitch to send messages",
            SendBlock::Reconnecting | SendBlock::Disconnected | SendBlock::ClientNotReady => {
                "Twitch chat send is not available"
            }
        }
    }

    fn clamp_text_cursor(text: &str, cursor_position: usize) -> usize {
        if cursor_position >= text.len() {
            return text.len();
        }
        if text.is_char_boundary(cursor_position) {
            return cursor_position;
        }
        text.char_indices()
            .map(|(idx, _)| idx)
            .take_while(|idx| *idx < cursor_position)
            .last()
            .unwrap_or(0)
    }

    fn reveal_composer_caret(
        &mut self,
        split_id: SplitId,
        layout: &ComposerTextLayout,
        cursor: usize,
        bounds: Bounds<Pixels>,
    ) -> Option<Bounds<Pixels>> {
        let viewport = self.composer_viewports.entry(split_id).or_default();
        let position = layout.cursor_position(cursor, viewport.caret_row)?;
        let height = bounds.size.height.as_f32();
        let content_height = layout.rows.len() as f32 * COMPOSER_LINE_HEIGHT_PX;
        let y = position.y.as_f32();
        viewport.offset_y = viewport.offset_y.min((content_height - height).max(0.0));
        if y < viewport.offset_y {
            viewport.offset_y = y;
        } else if y + COMPOSER_LINE_HEIGHT_PX > viewport.offset_y + height {
            viewport.offset_y = (y + COMPOSER_LINE_HEIGHT_PX - height).max(0.0);
        }
        let inset = Self::composer_top_inset(&layout.lines, height);
        Some(Bounds::new(
            point(
                bounds.left() + position.x,
                bounds.top() + px(inset + y - viewport.offset_y),
            ),
            size(px(1.5), px(COMPOSER_LINE_HEIGHT_PX - 1.0)),
        ))
    }

    fn render_chat_text(
        renderer: Entity<Self>,
        split_id: SplitId,
        input_text: &str,
        cursor_position: usize,
        selection: Option<Range<usize>>,
        marked_range: Option<Range<usize>>,
        is_placeholder: bool,
        caret_active: bool,
        caret_visible: bool,
        send_blocked: bool,
        placeholder_text: &str,
    ) -> AnyElement {
        if is_placeholder {
            return div()
                .h_full()
                .relative()
                .flex()
                .items_center()
                .when(caret_active, |el| {
                    el.child(
                        div()
                            .absolute()
                            .left_0()
                            .child(Self::render_input_cursor(caret_visible)),
                    )
                })
                .child(
                    div()
                        .text_size(px(COMPOSER_PRIMARY_TEXT_PX))
                        .text_color(if send_blocked {
                            colors::warning()
                        } else {
                            colors::text_muted()
                        })
                        .child(placeholder_text.to_string()),
                )
                .into_any_element();
        }
        let cursor = Self::clamp_text_cursor(input_text, cursor_position);
        let text = input_text.to_string();
        let valid_range = |range: &Range<usize>| {
            range.start < range.end
                && range.end <= text.len()
                && text.is_char_boundary(range.start)
                && text.is_char_boundary(range.end)
        };
        let selection = selection.filter(valid_range);
        let marked_range = marked_range.filter(valid_range);
        canvas(
            move |bounds, window, cx| {
                let font = window.text_style().font();
                let mut boundaries = vec![0, text.len()];
                for range in selection.iter().chain(marked_range.iter()) {
                    if range.start < range.end
                        && range.end <= text.len()
                        && text.is_char_boundary(range.start)
                        && text.is_char_boundary(range.end)
                    {
                        boundaries.extend([range.start, range.end]);
                    }
                }
                boundaries.sort_unstable();
                boundaries.dedup();
                let runs: Vec<_> = boundaries
                    .windows(2)
                    .map(|range| TextRun {
                        len: range[1] - range[0],
                        font: font.clone(),
                        color: colors::text_primary(),
                        background_color: selection
                            .as_ref()
                            .filter(|selection| selection.contains(&range[0]))
                            .map(|_| colors::accent_transparent(0.34)),
                        underline: marked_range
                            .as_ref()
                            .filter(|marked| marked.contains(&range[0]))
                            .map(|_| UnderlineStyle {
                                color: Some(colors::accent()),
                                thickness: px(1.0),
                                wavy: false,
                            }),
                        strikethrough: None,
                    })
                    .collect();
                let lines = window
                    .text_system()
                    .shape_text(
                        text.clone().into(),
                        px(COMPOSER_PRIMARY_TEXT_PX),
                        &runs,
                        Some(bounds.size.width),
                        None,
                    )
                    .ok()?;
                let layout = Arc::new(ComposerTextLayout::new(lines));
                let (caret, offset_y) = renderer.update(cx, |renderer, _| {
                    renderer.composer_text_bounds.insert(split_id, bounds);
                    if let Some(entry) = renderer.composer_layouts.get_mut(&split_id) {
                        if entry.input_text != text {
                            entry.input_text.clone_from(&text);
                        }
                        entry.font = font;
                        entry.width = bounds.size.width;
                        entry.layout = Arc::clone(&layout);
                    } else {
                        renderer.composer_layouts.insert(
                            split_id,
                            ComposerLayoutCacheEntry {
                                input_text: text.clone(),
                                font,
                                width: bounds.size.width,
                                layout: Arc::clone(&layout),
                            },
                        );
                    }
                    let caret = renderer.reveal_composer_caret(split_id, &layout, cursor, bounds);
                    let offset = renderer
                        .composer_viewports
                        .get(&split_id)
                        .map_or(0.0, |viewport| viewport.offset_y);
                    (caret, offset)
                });
                let origin_y = bounds.top()
                    + px(
                        Self::composer_top_inset(&layout.lines, bounds.size.height.as_f32())
                            - offset_y,
                    );
                Some((layout, caret, origin_y))
            },
            move |bounds, prepaint, window, cx| {
                let Some((layout, caret, mut y)) = prepaint else {
                    return;
                };
                window.with_content_mask(Some(ContentMask { bounds }), |window| {
                    for line in &layout.lines {
                        let height =
                            (line.wrap_boundaries().len() + 1) as f32 * COMPOSER_LINE_HEIGHT_PX;
                        if y + px(height) >= bounds.top() && y < bounds.bottom() {
                            let origin = point(bounds.left(), y);
                            let _ = line.paint_background(
                                origin,
                                px(COMPOSER_LINE_HEIGHT_PX),
                                TextAlign::Left,
                                Some(bounds),
                                window,
                                cx,
                            );
                            let _ = line.paint(
                                origin,
                                px(COMPOSER_LINE_HEIGHT_PX),
                                TextAlign::Left,
                                Some(bounds),
                                window,
                                cx,
                            );
                        }
                        y += px(height);
                    }
                    if caret_active
                        && caret_visible
                        && let Some(caret) = caret
                    {
                        window.paint_quad(fill(caret, colors::accent()));
                    }
                });
            },
        )
        .size_full()
        .into_any_element()
    }

    fn render_draft_preview_rail(preview: &DraftPreview) -> AnyElement {
        let mut rail = div()
            .max_w(px(DRAFT_PREVIEW_RAIL_WIDTH_PX))
            .h(px(DRAFT_PREVIEW_HEIGHT_PX))
            .flex_shrink_0()
            .flex()
            .items_center()
            .justify_end()
            .gap(px(DRAFT_PREVIEW_GAP_PX))
            .pl(px(DRAFT_PREVIEW_RAIL_PADDING_PX))
            .border_l(px(DRAFT_PREVIEW_RAIL_BORDER_PX))
            .border_color(colors::border_subtle())
            .overflow_hidden();

        for item in &preview.items {
            rail = rail.child(Self::render_draft_preview_image(item));
        }
        if preview.overflow_count > 0 {
            rail = rail.child(
                div()
                    .flex_shrink_0()
                    .whitespace_nowrap()
                    .text_size(px(COMPOSER_SECONDARY_TEXT_PX))
                    .text_color(colors::text_muted())
                    .child(format!("+{}", preview.overflow_count)),
            );
        }

        rail.into_any_element()
    }

    /// Render one bounded pre-send emote preview through the same identity-aware
    /// cache path as autocomplete. Animated media must use the current decoded
    /// frame instead of a raw image URL so it can advance in the composer rail.
    fn render_draft_preview_image(item: &DraftPreviewItem) -> AnyElement {
        let image_url: &str = item.image_url.as_ref();
        let placeholder = || {
            div()
                .id(item.element_id.clone())
                .w(px(item.width_px))
                .h(px(DRAFT_PREVIEW_HEIGHT_PX))
                .flex_shrink_0()
                .rounded(Radii::sm())
                .bg(colors::bg_hover())
                .opacity(0.48)
                .into_any_element()
        };

        match Self::resolve_composer_preview_image(
            image_url,
            item.is_animated,
            item.media_identity.as_ref(),
        ) {
            Some(render_image) => img(render_image)
                .id(item.element_id.clone())
                .w(px(item.width_px))
                .h(px(DRAFT_PREVIEW_HEIGHT_PX))
                .object_fit(ObjectFit::Contain)
                .into_any_element(),
            None => placeholder(),
        }
    }

    /// Resolve a composer emote preview image: the current animated frame,
    /// else the cached still, triggering an autocomplete-visible decode or
    /// preload on a miss.
    fn resolve_composer_preview_image(
        image_url: &str,
        is_animated: bool,
        media_identity: Option<&crate::cache::disk_layer::MediaSourceId>,
    ) -> Option<Arc<RenderImage>> {
        if is_animated {
            if let Some(frame) =
                image_cache::animated_frame(image_url, image_cache::FrameQuery::Current)
            {
                crate::emote_renderer::register_visible_metadata(frame.metadata);
                return Some(frame.image);
            }
            if let Some(render_image) = image_cache::get_cached_render_image(image_url) {
                return Some(render_image);
            }

            image_cache::trigger_animated_decode_media(
                image_url,
                media_identity.cloned(),
                image_cache::PreloadSource::AutocompleteVisible,
            );
            return None;
        }

        if let Some(render_image) = image_cache::get_cached_render_image(image_url) {
            return Some(render_image);
        }

        if !image_cache::is_downloading(image_url) {
            image_cache::trigger_preload_media_single(
                image_url,
                media_identity.cloned(),
                image_cache::PreloadSource::AutocompleteVisible,
            );
        }
        None
    }

    fn render_input_cursor(visible: bool) -> AnyElement {
        div()
            .w(px(1.5))
            .h(px(17.0))
            .bg(colors::accent())
            .opacity(if visible { 1.0 } else { 0.0 })
            .into_any_element()
    }

    /// Render inline autocomplete popup (positioned above input)
    fn render_inline_autocomplete(
        &mut self,
        channel: &str,
        split_id: SplitId,
        overlay_manager: Entity<OverlayManager>,
        app: Entity<crate::app::EchoApp>,
        autocomplete_a11y: Rc<RefCell<AutocompleteA11yTargets>>,
        cx: &mut Context<Self>,
    ) -> (AnyElement, bool) {
        if self.multiline_pastes.contains_key(&split_id) {
            return (div().into_any_element(), false);
        }
        let autocomplete_entity = overlay_manager.read(cx).autocomplete.clone();
        autocomplete_entity.update(cx, |autocomplete, _| {
            autocomplete.refresh_open_emote_suggestions();
        });
        let autocomplete = autocomplete_entity.read(cx);

        if !autocomplete.state.is_open
            || autocomplete.state.channel != channel
            || autocomplete.state.split_id != split_id
        {
            return (div().into_any_element(), false);
        }

        let scroll_y = (-autocomplete.scroll_handle().offset().y.as_f32()).max(0.0);
        let first_visible = (scroll_y / 34.0) as usize;
        let visible_count = AUTOCOMPLETE_VISIBLE_ROWS + usize::from(scroll_y % 34.0 > 0.0);
        autocomplete.prefetch_visible_suggestions(first_visible, visible_count);
        let visible_range = first_visible..first_visible.saturating_add(visible_count);
        let suggestions = autocomplete.suggestions.clone();
        let selected_idx = autocomplete.state.selected_index;
        let ac_type = autocomplete.state.ac_type;
        let command_argument_kind = (ac_type
            == crate::components::overlays::AutocompleteType::Command)
            .then(|| command_argument_kind_from_query(&autocomplete.state.query))
            .flatten();
        let suggestion_revision = autocomplete.suggestion_revision();
        let has_animated_suggestions = suggestions
            .iter()
            .skip(first_visible)
            .take(visible_count)
            .any(|suggestion| suggestion.is_animated && suggestion.image_url.is_some());
        self.composer_animation_urls.extend(
            suggestions
                .iter()
                .skip(first_visible)
                .take(visible_count)
                .filter(|suggestion| suggestion.is_animated)
                .filter_map(|suggestion| suggestion.image_url.as_deref().map(SharedString::from)),
        );

        let popup_height = if suggestions.is_empty() {
            AUTOCOMPLETE_EMPTY_ROW_HEIGHT_PX
        } else {
            suggestions
                .iter()
                .take(AUTOCOMPLETE_VISIBLE_ROWS)
                .map(|suggestion| {
                    Self::autocomplete_item_height(ac_type, suggestion.subtitle.is_some())
                })
                .sum()
        };

        let input_height = self
            .input_surface_metrics
            .get(&split_id)
            .map(|metrics| metrics.total_height)
            .unwrap_or(config::CHAT_INPUT_HEIGHT_PX);

        let mut popup = div()
            .id("autocomplete-inline")
            .role(Role::ListBox)
            .aria_label("Suggestions")
            .w_full()
            .a11y_synthetic_children({
                let targets = Rc::clone(&autocomplete_a11y);
                move |builder| {
                    let mut targets = targets.borrow_mut();
                    let options = builder.parent_node().children();
                    targets.count = options.len();
                    targets.active_id = options.get(selected_idx).copied();
                }
            })
            .max_h(px(popup_height))
            .bg(colors::bg_elevated())
            .rounded_t(Radii::sm())
            .border_1()
            .border_b_0()
            .border_color(colors::border_subtle())
            .shadow_lg()
            .overflow_y_scroll()
            .track_scroll(autocomplete.scroll_handle())
            .on_scroll(cx.listener(|_, _, _, cx| cx.notify()))
            .overflow_x_hidden()
            .occlude()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_mouse_down(MouseButton::Right, |_, _, cx| cx.stop_propagation())
            .on_mouse_down(MouseButton::Middle, |_, _, cx| cx.stop_propagation())
            .on_mouse_up(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_mouse_up(MouseButton::Right, |_, _, cx| cx.stop_propagation())
            .on_mouse_up(MouseButton::Middle, |_, _, cx| cx.stop_propagation())
            .on_mouse_move(|_, _, cx| cx.stop_propagation())
            .on_scroll(|_, _, cx| cx.stop_propagation());

        if suggestions.is_empty() {
            let empty_copy = match ac_type {
                crate::components::overlays::AutocompleteType::Emote => "No matching emotes",
                crate::components::overlays::AutocompleteType::Mention => {
                    "No matching recent chatters"
                }
                crate::components::overlays::AutocompleteType::Command => {
                    match command_argument_kind {
                        Some(CommandArgumentKind::User) => "No matching chatters",
                        Some(CommandArgumentKind::Channel) => "No matching channels",
                        Some(CommandArgumentKind::MessageId) => "No matching messages",
                        Some(CommandArgumentKind::PredictionOutcome) => "No matching outcomes",
                        Some(CommandArgumentKind::Option | CommandArgumentKind::Value) => {
                            "No matching command values"
                        }
                        None => "No matching commands",
                    }
                }
                crate::components::overlays::AutocompleteType::Tab => {
                    "No matching emotes or chatters"
                }
            };
            popup = popup.child(
                div()
                    .id("autocomplete-empty-status")
                    .w_full()
                    .h(px(AUTOCOMPLETE_EMPTY_ROW_HEIGHT_PX))
                    .px(px(Spacing::SM_PLUS))
                    .flex()
                    .items_center()
                    .cursor_default()
                    .text_size(px(AUTOCOMPLETE_SECONDARY_TEXT_PX))
                    .text_color(colors::text_muted())
                    .child(empty_copy),
            );
        } else {
            popup = popup.children(suggestions.iter().enumerate().map(|(idx, suggestion)| {
                let is_selected = idx == selected_idx;
                self.render_autocomplete_item(
                    suggestion,
                    is_selected,
                    visible_range.contains(&idx),
                    ac_type,
                    idx,
                    suggestion_revision,
                    app.clone(),
                    cx,
                )
            }));
        }

        // The popup prepaints before the composer, so both callbacks expose real node IDs.
        let popup = div()
            .id("autocomplete-popup")
            .role(Role::Group)
            .absolute()
            .left_0()
            .w(px(AUTOCOMPLETE_POPUP_WIDTH_PX))
            .max_w_full()
            .bottom(px(input_height))
            .child(popup)
            .a11y_synthetic_children(move |builder| {
                autocomplete_a11y.borrow_mut().popup_id =
                    builder.parent_node().children().first().copied();
            });
        (popup.into_any_element(), has_animated_suggestions)
    }

    fn autocomplete_item_height(
        ac_type: crate::components::overlays::AutocompleteType,
        has_subtitle: bool,
    ) -> f32 {
        use crate::components::overlays::AutocompleteType;

        match ac_type {
            AutocompleteType::Command if has_subtitle => 36.0,
            AutocompleteType::Command => 34.0,
            AutocompleteType::Emote | AutocompleteType::Mention | AutocompleteType::Tab => 34.0,
        }
    }

    /// Render a single autocomplete suggestion item
    fn render_autocomplete_item(
        &self,
        suggestion: &crate::components::overlays::AutocompleteSuggestion,
        is_selected: bool,
        preview_visible: bool,
        ac_type: crate::components::overlays::AutocompleteType,
        idx: usize,
        suggestion_revision: u64,
        app: Entity<crate::app::EchoApp>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        use crate::components::overlays::AutocompleteType;

        let is_mention_suggestion = ac_type == AutocompleteType::Mention
            || (suggestion.emote_type.is_none() && suggestion.text.starts_with('@'));
        let display_text = if is_mention_suggestion {
            suggestion
                .text
                .strip_prefix('@')
                .unwrap_or(&suggestion.text)
                .to_string()
        } else {
            suggestion.text.clone()
        };
        let accessible_label = if suggestion.emote_type.is_some() {
            format!(
                "{}, {} emote",
                suggestion.text,
                suggestion.provider.as_deref().unwrap_or("chat")
            )
        } else if suggestion.text.starts_with('/') {
            format!("{} — command", suggestion.text)
        } else {
            suggestion.text.clone()
        };
        let image_url = suggestion.image_url.clone();
        let subtitle = suggestion.subtitle.clone();
        let color = suggestion.color.clone();
        let provider = suggestion.provider.clone();
        let emote_type = suggestion.emote_type;
        let is_animated = suggestion.is_animated;
        let item_height = Self::autocomplete_item_height(ac_type, subtitle.is_some());
        let leading_visual = if !preview_visible && image_url.is_some() {
            div()
                .size(px(AUTOCOMPLETE_EMOTE_SIZE_PX))
                .flex_shrink_0()
                .into_any_element()
        } else if let Some(url) = image_url {
            Self::render_autocomplete_emote_preview(
                &url,
                is_animated,
                suggestion.media_identity.as_ref(),
            )
        } else if is_mention_suggestion {
            div()
                .size(px(AUTOCOMPLETE_EMOTE_SIZE_PX))
                .flex_shrink_0()
                .flex()
                .items_center()
                .justify_center()
                .rounded(Radii::md())
                .bg(colors::accent_transparent(0.08))
                .child(div().text_sm().text_color(colors::accent_text()).child("@"))
                .into_any_element()
        } else {
            div().into_any_element()
        };

        let content = if let Some(scope) = emote_type {
            div()
                .w_full()
                .flex()
                .items_center()
                .gap(px(Spacing::SM))
                .child(leading_visual)
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .overflow_hidden()
                        .text_ellipsis()
                        .whitespace_nowrap()
                        .text_size(px(AUTOCOMPLETE_PRIMARY_TEXT_PX))
                        .font_weight(if is_selected {
                            FontWeight::MEDIUM
                        } else {
                            FontWeight::NORMAL
                        })
                        .when_some(color.clone(), |s, c| {
                            if let Some(rgb) = crate::utils::color::parse_hex_color_opt(&c) {
                                s.text_color(rgb)
                            } else {
                                s.text_color(colors::text_primary())
                            }
                        })
                        .when(color.is_none(), |s| s.text_color(colors::text_primary()))
                        .child(display_text),
                )
                .child(Self::render_autocomplete_origin_label(
                    provider.as_deref(),
                    scope,
                ))
                .into_any_element()
        } else {
            let metadata = subtitle.or_else(|| {
                if is_mention_suggestion {
                    Some("Recent chatter".to_string())
                } else {
                    None
                }
            });

            div()
                .w_full()
                .flex()
                .items_center()
                .gap(px(Spacing::SM))
                .child(leading_visual)
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .justify_center()
                        .overflow_hidden()
                        .child(
                            div()
                                .text_size(px(AUTOCOMPLETE_PRIMARY_TEXT_PX))
                                .font_weight(if is_selected {
                                    FontWeight::MEDIUM
                                } else {
                                    FontWeight::NORMAL
                                })
                                .overflow_hidden()
                                .text_ellipsis()
                                .whitespace_nowrap()
                                .when_some(color.clone(), |s, c| {
                                    if let Some(rgb) = crate::utils::color::parse_hex_color_opt(&c)
                                    {
                                        s.text_color(rgb)
                                    } else {
                                        s.text_color(colors::text_primary())
                                    }
                                })
                                .when(color.is_none(), |s| s.text_color(colors::text_primary()))
                                .child(display_text),
                        )
                        .when_some(metadata, |s, detail| {
                            s.child(
                                div()
                                    .text_size(px(AUTOCOMPLETE_SECONDARY_TEXT_PX))
                                    .text_color(colors::text_muted())
                                    .overflow_hidden()
                                    .text_ellipsis()
                                    .whitespace_nowrap()
                                    .child(detail),
                            )
                        }),
                )
                .when_some(provider, |row, provider| {
                    row.child(Self::render_autocomplete_provider_label(&provider))
                })
                .into_any_element()
        };

        div()
            .id(("ac-item", idx))
            .role(Role::ListBoxOption)
            .aria_label(accessible_label)
            .aria_selected(is_selected)
            .w_full()
            .h(px(item_height))
            .pl(px(Spacing::SM_PLUS))
            .pr(px(Spacing::SM))
            .relative()
            .flex()
            .items_center()
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
            .hover(move |s| {
                s.bg(if is_selected {
                    colors::accent_transparent(0.18)
                } else {
                    colors::bg_hover()
                })
            })
            .on_activate_without_tab_stop(cx.listener(move |_this, _, _window, cx| {
                let app = app.clone();
                cx.defer(move |cx| {
                    app.update(cx, |app_instance, cx| {
                        app_instance.accept_autocomplete(suggestion_revision, idx, cx);
                    });
                });
            }))
            .child(content)
    }

    fn render_autocomplete_emote_preview(
        image_url: &str,
        is_animated: bool,
        media_identity: Option<&crate::cache::disk_layer::MediaSourceId>,
    ) -> AnyElement {
        match Self::resolve_composer_preview_image(image_url, is_animated, media_identity) {
            Some(render_image) => img(render_image)
                .size(px(AUTOCOMPLETE_EMOTE_SIZE_PX))
                .flex_shrink_0()
                .object_fit(ObjectFit::Contain)
                .into_any_element(),
            None => div()
                .size(px(AUTOCOMPLETE_EMOTE_SIZE_PX))
                .flex_shrink_0()
                .rounded(Radii::md())
                .bg(colors::bg_hover())
                .opacity(0.48)
                .into_any_element(),
        }
    }

    fn render_autocomplete_origin_label(
        provider: Option<&str>,
        scope: crate::components::overlays::EmoteType,
    ) -> AnyElement {
        let label = provider
            .map(|provider| format!("{provider} · {}", scope.label()))
            .unwrap_or_else(|| scope.label().to_string());
        div()
            .flex_shrink_0()
            .max_w(px(96.0))
            .text_size(px(AUTOCOMPLETE_SECONDARY_TEXT_PX))
            .text_color(colors::text_muted())
            .overflow_hidden()
            .text_ellipsis()
            .whitespace_nowrap()
            .child(label)
            .into_any_element()
    }

    fn render_autocomplete_provider_label(provider: &str) -> AnyElement {
        div()
            .flex_shrink_0()
            .max_w(px(96.0))
            .text_size(px(AUTOCOMPLETE_SECONDARY_TEXT_PX))
            .text_color(colors::text_muted())
            .overflow_hidden()
            .text_ellipsis()
            .whitespace_nowrap()
            .child(provider.to_string())
            .into_any_element()
    }
}

impl ChatInputRenderer {
    fn utf16_offset_to_utf8_floor(text: &str, offset: usize) -> usize {
        let mut utf8_offset = 0;
        let mut utf16_count = 0;
        for ch in text.chars() {
            let next_utf16 = utf16_count + ch.len_utf16();
            if offset <= utf16_count {
                return utf8_offset;
            }
            if offset < next_utf16 {
                return utf8_offset;
            }
            utf16_count = next_utf16;
            utf8_offset += ch.len_utf8();
        }
        utf8_offset
    }

    fn utf16_offset_to_utf8_ceil(text: &str, offset: usize) -> usize {
        let mut utf8_offset = 0;
        let mut utf16_count = 0;
        for ch in text.chars() {
            if offset <= utf16_count {
                return utf8_offset;
            }
            let next_utf16 = utf16_count + ch.len_utf16();
            let next_utf8 = utf8_offset + ch.len_utf8();
            if offset <= next_utf16 {
                return next_utf8;
            }
            utf16_count = next_utf16;
            utf8_offset = next_utf8;
        }
        utf8_offset
    }

    fn utf8_offset_to_utf16(text: &str, offset: usize) -> usize {
        let mut utf8_count = 0;
        let mut utf16_offset = 0;
        for ch in text.chars() {
            if utf8_count >= offset {
                break;
            }
            utf8_count += ch.len_utf8();
            utf16_offset += ch.len_utf16();
        }
        utf16_offset
    }

    fn utf16_range_to_utf8(text: &str, range: Range<usize>) -> Range<usize> {
        if range.start == range.end {
            let offset = Self::utf16_offset_to_utf8_floor(text, range.start);
            return offset..offset;
        }
        Self::utf16_offset_to_utf8_floor(text, range.start)
            ..Self::utf16_offset_to_utf8_ceil(text, range.end)
    }

    fn utf8_range_to_utf16(text: &str, range: Range<usize>) -> Range<usize> {
        Self::utf8_offset_to_utf16(text, range.start)..Self::utf8_offset_to_utf16(text, range.end)
    }

    /// Type text into a split's draft (or its channel-placeholder field) on
    /// behalf of a keystroke that arrived while the input did not own focus.
    /// Returns the updated draft for autocomplete refresh.
    pub(crate) fn insert_typed_text(
        &mut self,
        split_id: SplitId,
        text: &str,
        placeholder: bool,
        cx: &mut Context<Self>,
    ) -> Option<String> {
        cx.notify();
        if placeholder {
            return None;
        }
        self.input_manager.commit_ime_composition(split_id);
        self.input_manager
            .replace_text_range(split_id, None, text, true)?;
        self.reset_composer_affinity(split_id);
        self.ime_marked_ranges.remove(&split_id);
        self.reset_inline_tab_completion(split_id);
        self.input_manager.end_history_navigation(split_id);
        self.input_manager.draft(split_id).cloned()
    }

    fn finish_native_text_edit(&mut self, split_id: SplitId, cx: &mut Context<Self>) {
        self.reset_composer_affinity(split_id);
        self.reset_inline_tab_completion(split_id);
        self.input_manager.end_history_navigation(split_id);
        let Some(target) = self.ime_targets.get(&split_id).cloned() else {
            cx.notify();
            return;
        };
        let current_text = self
            .input_manager
            .draft(split_id)
            .cloned()
            .unwrap_or_default();
        cx.defer(move |cx| {
            target.app.update(cx, |app, cx| {
                if app.split_layout.read(cx).is_unified_input_mode() {
                    cx.notify();
                } else {
                    app.notify_split_leaf(split_id, cx);
                }
                app.update_autocomplete(
                    target.channel.as_ref(),
                    target.path.as_ref(),
                    Some(&current_text),
                    cx,
                );
            });
        });
        cx.notify();
    }

    fn native_text_for_range(
        &mut self,
        split_id: SplitId,
        range_utf16: Range<usize>,
        adjusted_range: &mut Option<Range<usize>>,
    ) -> Option<String> {
        if !self.native_input_is_active(split_id) {
            return None;
        }
        let text = self.input_manager.text_for_split(split_id);
        let range = Self::utf16_range_to_utf8(text, range_utf16);
        adjusted_range.replace(Self::utf8_range_to_utf16(text, range.clone()));
        text.get(range).map(str::to_string)
    }

    fn native_selected_text_range(
        &mut self,
        split_id: SplitId,
        _ignore_disabled_input: bool,
    ) -> Option<UTF16Selection> {
        if !self.native_input_is_active(split_id) {
            return None;
        }
        let text = self.input_manager.text_for_split(split_id);
        let cursor = self.input_manager.cursor_for_split(split_id);
        let selection = self.input_manager.selection_for_split(split_id);
        let range = selection.clone().unwrap_or(cursor..cursor);
        Some(UTF16Selection {
            range: Self::utf8_range_to_utf16(text, range.clone()),
            reversed: selection.is_some_and(|_| cursor == range.start),
        })
    }

    fn native_marked_text_range(&self, split_id: SplitId) -> Option<Range<usize>> {
        if !self.native_input_is_active(split_id) {
            return None;
        }
        let text = self.input_manager.text_for_split(split_id);
        self.ime_marked_ranges
            .get(&split_id)
            .cloned()
            .map(|range| Self::utf8_range_to_utf16(text, range))
    }

    fn native_unmark_text(&mut self, split_id: SplitId, cx: &mut Context<Self>) {
        if !self.native_input_is_active(split_id) {
            return;
        }
        self.input_manager.commit_ime_composition(split_id);
        self.ime_marked_ranges.remove(&split_id);
        cx.notify();
    }

    /// Resolve an IME edit's UTF-8 target range, falling back to the marked range.
    fn resolve_ime_edit_range(
        &self,
        split_id: SplitId,
        range_utf16: Option<Range<usize>>,
    ) -> Option<Range<usize>> {
        let current_text = self.input_manager.text_for_split(split_id);
        range_utf16
            .map(|range| Self::utf16_range_to_utf8(current_text, range))
            .or_else(|| self.ime_marked_ranges.get(&split_id).cloned())
    }

    fn native_replace_text_in_range(
        &mut self,
        split_id: SplitId,
        range_utf16: Option<Range<usize>>,
        text: &str,
        cx: &mut Context<Self>,
    ) {
        if !self.native_input_is_active(split_id) {
            return;
        }
        let range = self.resolve_ime_edit_range(split_id, range_utf16);
        let marked_range = self.ime_marked_ranges.get(&split_id);
        let composing = marked_range.is_some();
        if composing && text.is_empty() && range.as_ref() == marked_range {
            self.input_manager.cancel_ime_composition(split_id);
            self.ime_marked_ranges.remove(&split_id);
            self.finish_native_text_edit(split_id, cx);
            return;
        }
        if self
            .input_manager
            .replace_text_range(split_id, range, text, !composing)
            .is_some()
        {
            self.input_manager.commit_ime_composition(split_id);
            self.ime_marked_ranges.remove(&split_id);
            self.finish_native_text_edit(split_id, cx);
        }
    }

    fn native_replace_and_mark_text_in_range(
        &mut self,
        split_id: SplitId,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        new_selected_range_utf16: Option<Range<usize>>,
        cx: &mut Context<Self>,
    ) {
        if !self.native_input_is_active(split_id) {
            return;
        }
        if new_text.is_empty() {
            self.input_manager.cancel_ime_composition(split_id);
            self.ime_marked_ranges.remove(&split_id);
            self.finish_native_text_edit(split_id, cx);
            return;
        }
        let range = self.resolve_ime_edit_range(split_id, range_utf16);
        self.input_manager.begin_ime_composition(split_id);
        let Some(inserted_range) = self
            .input_manager
            .replace_text_range(split_id, range, new_text, false)
        else {
            return;
        };
        self.ime_marked_ranges.remove(&split_id);
        self.ime_marked_ranges
            .insert(split_id, inserted_range.clone());
        if let Some(selection_utf16) = new_selected_range_utf16 {
            let relative = Self::utf16_range_to_utf8(new_text, selection_utf16);
            self.input_manager.set_pointer_selection(
                split_id,
                inserted_range.start + relative.start,
                inserted_range.start + relative.end,
            );
        }
        self.finish_native_text_edit(split_id, cx);
    }

    fn native_bounds_for_range(
        &mut self,
        split_id: SplitId,
        range_utf16: Range<usize>,
        element_bounds: Bounds<Pixels>,
        window: &mut Window,
    ) -> Option<Bounds<Pixels>> {
        if !self.native_input_is_active(split_id) {
            return None;
        }
        let text = self.input_manager.text_for_split(split_id);
        let range = Self::utf16_range_to_utf8(text, range_utf16);
        let bounds = self
            .composer_text_bounds
            .get(&split_id)
            .copied()
            .unwrap_or(element_bounds);
        let layout = self.composer_layout(split_id, window)?;
        let cursor = self.input_manager.cursor_for_split(split_id);
        self.reveal_composer_caret(split_id, &layout, cursor, bounds);
        let viewport = self.composer_viewports.get(&split_id)?;
        let position = layout.cursor_position(range.end, viewport.caret_row)?;
        Some(Bounds::new(
            point(
                bounds.left() + position.x,
                bounds.top()
                    + position.y
                    + px(
                        Self::composer_top_inset(&layout.lines, bounds.size.height.as_f32())
                            - viewport.offset_y,
                    ),
            ),
            size(px(1.5), px(COMPOSER_LINE_HEIGHT_PX - 1.0)),
        ))
    }

    fn native_character_index_for_point(
        &mut self,
        split_id: SplitId,
        point: Point<Pixels>,
        window: &mut Window,
    ) -> Option<usize> {
        if !self.native_input_is_active(split_id) {
            return None;
        }
        let index = self.composer_cursor_for_position(split_id, point, window)?;
        let text = self.input_manager.text_for_split(split_id);
        Some(Self::utf8_offset_to_utf16(text, index))
    }

    fn native_set_selected_text_range(
        &mut self,
        split_id: SplitId,
        range_utf16: Range<usize>,
        cx: &mut Context<Self>,
    ) {
        if !self.native_input_is_active(split_id) {
            return;
        }
        let text = self
            .input_manager
            .draft(split_id)
            .cloned()
            .unwrap_or_default();
        let range = Self::utf16_range_to_utf8(&text, range_utf16);
        if self
            .input_manager
            .set_pointer_selection(split_id, range.start, range.end)
        {
            self.reset_composer_affinity(split_id);
            cx.notify();
        }
    }

    fn native_text_length_utf16(&mut self, split_id: SplitId) -> Option<usize> {
        if !self.native_input_is_active(split_id) {
            return None;
        }
        Some(
            self.input_manager
                .draft(split_id)
                .map(|text| text.encode_utf16().count())
                .unwrap_or(0),
        )
    }
}

impl EntityInputHandler for ComposerInputHandler {
    fn text_for_range(
        &mut self,
        range_utf16: Range<usize>,
        adjusted_range: &mut Option<Range<usize>>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<String> {
        self.renderer.update(cx, |renderer, _cx| {
            renderer.native_text_for_range(self.split_id, range_utf16, adjusted_range)
        })
    }

    fn selected_text_range(
        &mut self,
        ignore_disabled_input: bool,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        self.renderer.update(cx, |renderer, _cx| {
            renderer.native_selected_text_range(self.split_id, ignore_disabled_input)
        })
    }

    fn marked_text_range(
        &self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Range<usize>> {
        self.renderer.update(cx, |renderer, _cx| {
            renderer.native_marked_text_range(self.split_id)
        })
    }

    fn unmark_text(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        self.renderer.update(cx, |renderer, cx| {
            renderer.native_unmark_text(self.split_id, cx);
        });
    }

    fn replace_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        text: &str,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.renderer.update(cx, |renderer, cx| {
            renderer.native_replace_text_in_range(self.split_id, range_utf16, text, cx);
        });
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        new_selected_range_utf16: Option<Range<usize>>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.renderer.update(cx, |renderer, cx| {
            renderer.native_replace_and_mark_text_in_range(
                self.split_id,
                range_utf16,
                new_text,
                new_selected_range_utf16,
                cx,
            );
        });
    }

    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        element_bounds: Bounds<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        self.renderer.update(cx, |renderer, _cx| {
            renderer.native_bounds_for_range(self.split_id, range_utf16, element_bounds, window)
        })
    }

    fn character_index_for_point(
        &mut self,
        point: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<usize> {
        self.renderer.update(cx, |renderer, _cx| {
            renderer.native_character_index_for_point(self.split_id, point, window)
        })
    }

    fn set_selected_text_range(
        &mut self,
        range_utf16: Range<usize>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.renderer.update(cx, |renderer, cx| {
            renderer.native_set_selected_text_range(self.split_id, range_utf16, cx);
        });
    }

    fn text_length_utf16(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> Option<usize> {
        self.renderer.update(cx, |renderer, _cx| {
            renderer.native_text_length_utf16(self.split_id)
        })
    }

    fn accepts_text_input(&self, _window: &mut Window, _cx: &mut Context<Self>) -> bool {
        self.renderer.update(_cx, |renderer, _cx| {
            renderer.native_input_is_active(self.split_id)
        })
    }
}
