//! Disposable state used by the opt-in native UI interaction harness.
//!
//! This module must never affect an ordinary Echo session. The fixture is
//! enabled only by `ECHO_BENCH_UI_FIXTURE=1`, changes only in-memory state, and
//! deliberately avoids the normal tab/settings persistence paths.

use crate::backend::{StreamInfo, TabOverflowMode, TabTheme};
use crate::state::{AppState, SplitNode, SplitOrientation, TabHighlightState, TabState};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;

pub const OFFLINE_CHANNEL: &str = "echo-offline-fixture";
pub const LIVE_CHANNEL: &str = "echo-live-fixture";
pub const HIGHLIGHT_LIVE_CHANNEL: &str = "echo-highlight-live-fixture";
pub const LIFECYCLE_CHURN_CHANNELS: [&str; 2] =
    ["echo-lifecycle-fixture-a", "echo-lifecycle-fixture-b"];
pub const FIXTURE_TAB_COUNT: usize = 8;
pub const FIXTURE_SPLIT_COUNT: usize = 2;
pub const FIXTURE_ROOMSTATE_LABEL_COUNT: usize = 5;
pub const SAFE_HOVER_TAB_INDEX: usize = 4;
pub const FIXTURE_TAB_LAYOUT_WIDTH_PX: f32 = 720.0;
pub const COMPLETION_EMOTES: [&str; 3] = ["yeEchoAlpha", "yeEchoBeta", "yeEchoGamma"];
pub const COMPLETION_CHATTERS: [(&str, &str); 3] = [
    ("echofixtureuseralice", "EchoFixtureUserAlice"),
    ("echofixtureuserbob", "EchoFixtureUserBob"),
    ("echofixtureusercara", "EchoFixtureUserCara"),
];
/// Mixed LTR/RTL text used only by the opt-in native shared-Input fixture.
///
/// The driver seeds this directly into the channel-popup `InputState`; it
/// never injects characters or presses Send.
///
/// Paint-time shaping chooses the first UTF-8-boundary range whose exact
/// visual geometry is discontiguous; the native harness reproduces that
/// published range without typing or sending.
pub const FEEDBACK_BIDI_INPUT_TEXT: &str = "abcdאבגX";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompletionUiSnapshot {
    pub input: String,
    pub cursor: usize,
    pub focused: bool,
    pub suggestions_open: bool,
    pub suggestion_count: usize,
    pub selected_text: Option<String>,
}

const OVERFLOW_SCROLL: u8 = 0;
const OVERFLOW_DROPDOWN: u8 = 1;
const OVERFLOW_MULTILINE: u8 = 2;

static RUNTIME_OVERFLOW_MODE: AtomicU8 = AtomicU8::new(OVERFLOW_SCROLL);
static ENABLED: LazyLock<bool> =
    LazyLock::new(|| env_flag("ECHO_BENCH_UI_FIXTURE") || responsive_matrix_enabled());
static SINGLE_INSTANCE_BYPASS_ENABLED: LazyLock<bool> = LazyLock::new(|| {
    is_enabled() || link_preview_matrix_enabled() || env_flag("ECHO_BENCH_ALLOW_MULTI_INSTANCE")
});
static RESPONSIVE_MATRIX_ENABLED: LazyLock<bool> =
    LazyLock::new(|| env_flag("ECHO_BENCH_RESPONSIVE_MATRIX"));
static FEEDBACK_ENABLED: LazyLock<bool> = LazyLock::new(|| env_flag("ECHO_BENCH_FEEDBACK_UI"));

#[derive(Clone, Copy, Debug, Default)]
pub struct FeedbackRect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct FeedbackComposerSnapshot {
    pub line_count: usize,
    pub composer_height: f32,
    pub text_height: f32,
    pub action_height: f32,
    pub overlap_safe: bool,
    pub clipped: bool,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct FeedbackComposerPointerSnapshot {
    pub bounds: FeedbackRect,
    pub text_len: usize,
    pub cursor: usize,
    pub selection_start: Option<usize>,
    pub selection_end: Option<usize>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FeedbackCaretAffinity {
    #[default]
    Leading,
    Trailing,
}

impl FeedbackCaretAffinity {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Leading => "leading",
            Self::Trailing => "trailing",
        }
    }
}

/// Paint-time geometry captured from the shared production [`Input`] element.
///
/// `selection_spans` deliberately preserves every exact visual span rather
/// than reducing mixed-direction text to endpoints.
#[derive(Clone, Debug, Default)]
pub struct FeedbackSharedInputSnapshot {
    pub bounds: FeedbackRect,
    pub text_len: usize,
    pub cursor: usize,
    pub cursor_affinity: FeedbackCaretAffinity,
    /// Exact fixture range proven discontiguous by `visual_spans_for_range`.
    pub target_selection_start: Option<usize>,
    pub target_selection_end: Option<usize>,
    pub target_selection_start_affinity: Option<FeedbackCaretAffinity>,
    pub target_selection_end_affinity: Option<FeedbackCaretAffinity>,
    /// Interior x coordinates relative to `bounds` whose shaped-line hit
    /// testing resolves to the published fixture selection boundaries.
    pub selection_start_x: Option<f32>,
    pub selection_end_x: Option<f32>,
    pub selection_start: Option<usize>,
    pub selection_end: Option<usize>,
    pub selection_spans: Vec<FeedbackRect>,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct FeedbackChannelChooserSnapshot {
    pub split_id: u64,
    pub input_focused: bool,
    pub query_empty: bool,
    pub placeholder_present: bool,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct FeedbackHeaderSnapshot {
    pub observations: usize,
    pub overlap: bool,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct FeedbackSoundSliderSnapshot {
    pub bounds: FeedbackRect,
    pub value: f32,
    pub enabled: bool,
    pub hovered: bool,
    pub clicked_value: Option<f32>,
}

#[derive(Debug, Default)]
struct FeedbackUiState {
    channel_chooser: Option<FeedbackChannelChooserSnapshot>,
    composer: Option<FeedbackComposerSnapshot>,
    composer_pointer: Option<FeedbackComposerPointerSnapshot>,
    shared_input: Option<FeedbackSharedInputSnapshot>,
    headers: HashMap<u64, bool>,
    sound_slider: Option<FeedbackSoundSliderSnapshot>,
}

static FEEDBACK_UI_STATE: LazyLock<Mutex<FeedbackUiState>> =
    LazyLock::new(|| Mutex::new(FeedbackUiState::default()));

fn feedback_state() -> std::sync::MutexGuard<'static, FeedbackUiState> {
    FEEDBACK_UI_STATE.lock().unwrap_or_else(|e| e.into_inner())
}

static FEEDBACK_BIDI_INPUT_REQUESTED: AtomicBool = AtomicBool::new(false);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UiFixtureSemanticSnapshot {
    pub tabs: usize,
    pub splits: usize,
    pub offline_status: &'static str,
    pub offline_title: bool,
    pub offline_category: bool,
    pub live_status: &'static str,
    pub live_title: bool,
    pub live_category: bool,
    pub live_viewers: u32,
    pub roomstate_labels: usize,
    pub selected_tabs: usize,
    pub highlighted_tabs: usize,
    pub new_message_tabs: usize,
    pub unread_fallback_tabs: usize,
    pub live_tabs: usize,
    pub long_title_tabs: usize,
}

impl UiFixtureSemanticSnapshot {
    pub fn is_complete(&self) -> bool {
        self.tabs == FIXTURE_TAB_COUNT
            && self.splits == FIXTURE_SPLIT_COUNT
            && self.offline_status == "OFFLINE"
            && self.offline_title
            && self.offline_category
            && self.live_status == "LIVE"
            && self.live_title
            && self.live_category
            && self.live_viewers == 12_345
            && self.roomstate_labels == FIXTURE_ROOMSTATE_LABEL_COUNT
            && self.selected_tabs == 1
            && self.highlighted_tabs == 2
            && self.new_message_tabs == 1
            && self.unread_fallback_tabs == 1
            && self.live_tabs == 2
            && self.long_title_tabs == 1
    }
}

/// Return whether the disposable UI fixture is explicitly enabled.
pub fn is_enabled() -> bool {
    *ENABLED
}

pub fn disposable_workspace_enabled() -> bool {
    is_enabled()
}

/// Return whether the deterministic in-process lifecycle churn driver is active.
/// This remains separate from the UI fixture flag so the UI interaction
/// driver and its log contract are unchanged.
pub fn lifecycle_churn_enabled() -> bool {
    is_enabled() && env_flag("ECHO_BENCH_LIFECYCLE_CHURN")
}

/// Return whether the isolated live no-send link-preview matrix is active.
/// This intentionally does not enable the broader fixture workspace: the
/// matrix must retain the ordinary one-channel live connection lifecycle.
pub fn link_preview_matrix_enabled() -> bool {
    env_flag("ECHO_BENCH_LINK_PREVIEW_MATRIX")
}

/// Return whether the non-persisting responsive screenshot workspace is active.
pub fn responsive_matrix_enabled() -> bool {
    *RESPONSIVE_MATRIX_ENABLED
}

/// Whether a disposable benchmark process may run beside the user's ordinary
/// Echo instance.
///
/// The explicit environment gate is intentionally narrower than `is_enabled`:
/// live no-send picker profiles need real saved-layout state, so enabling the
/// UI fixture merely to bypass the Windows mutex changes the workload and
/// prevents the picker driver from running.
pub fn single_instance_bypass_enabled() -> bool {
    *SINGLE_INSTANCE_BYPASS_ENABLED
}

pub(crate) fn env_flag(name: &str) -> bool {
    std::env::var(name)
        .map(|value| matches!(value.as_str(), "1" | "true" | "TRUE" | "yes" | "YES"))
        .unwrap_or(false)
}

pub(crate) fn env_u64(name: &str) -> Option<u64> {
    std::env::var(name).ok().and_then(|v| v.parse::<u64>().ok())
}

/// Return whether a channel belongs to the disposable fixture workspace.
///
/// Provider, history, and self-heal paths use this boundary to keep synthetic
/// channel names entirely process-local while the explicit fixture is active.
pub fn is_fixture_channel(channel: &str) -> bool {
    if !disposable_workspace_enabled() {
        return false;
    }
    let channel = channel.trim_start_matches('#').to_ascii_lowercase();
    channel.starts_with("echo-") && channel.ends_with("-fixture")
}

/// Renderer checkpoints are active only for the explicit feedback fixture.
pub fn feedback_ui_enabled() -> bool {
    is_enabled() && *FEEDBACK_ENABLED
}

pub fn reset_feedback_ui_snapshots() {
    if feedback_ui_enabled() {
        *feedback_state() = FeedbackUiState::default();
    }
}

/// Arm the next rendered shared [`Input`] surface for the deterministic
/// mixed-direction fixture. The request is one-shot, feedback-gated, and is
/// consumed by the isolated fixture channel popup while no other shared input
/// is visible.
pub fn request_feedback_bidi_input() -> bool {
    if !feedback_ui_enabled() {
        return false;
    }
    FEEDBACK_BIDI_INPUT_REQUESTED.store(true, Ordering::Release);
    true
}

/// Consume the one-shot shared-input seed request.
pub fn take_feedback_bidi_input_request() -> bool {
    feedback_ui_enabled() && FEEDBACK_BIDI_INPUT_REQUESTED.swap(false, Ordering::AcqRel)
}

pub fn is_feedback_bidi_input_text(text: &str) -> bool {
    feedback_ui_enabled() && text == FEEDBACK_BIDI_INPUT_TEXT
}

pub fn publish_feedback_shared_input(snapshot: FeedbackSharedInputSnapshot) {
    if feedback_ui_enabled() {
        feedback_state().shared_input = Some(snapshot);
    }
}

pub fn feedback_shared_input_snapshot() -> Option<FeedbackSharedInputSnapshot> {
    feedback_ui_enabled().then(|| feedback_state().shared_input.clone())?
}

pub fn reset_feedback_header_snapshot() {
    if feedback_ui_enabled() {
        feedback_state().headers.clear();
    }
}

pub fn publish_feedback_composer(snapshot: FeedbackComposerSnapshot) {
    if feedback_ui_enabled() {
        feedback_state().composer = Some(snapshot);
    }
}

pub fn publish_feedback_channel_chooser(snapshot: FeedbackChannelChooserSnapshot) {
    if feedback_ui_enabled() {
        feedback_state().channel_chooser = Some(snapshot);
    }
}

pub fn feedback_channel_chooser_snapshot() -> Option<FeedbackChannelChooserSnapshot> {
    feedback_ui_enabled().then(|| feedback_state().channel_chooser)?
}

pub fn feedback_composer_snapshot() -> Option<FeedbackComposerSnapshot> {
    feedback_ui_enabled().then(|| feedback_state().composer)?
}

pub fn publish_feedback_composer_pointer(snapshot: FeedbackComposerPointerSnapshot) {
    if feedback_ui_enabled() {
        feedback_state().composer_pointer = Some(snapshot);
    }
}

pub fn feedback_composer_pointer_snapshot() -> Option<FeedbackComposerPointerSnapshot> {
    feedback_ui_enabled().then(|| feedback_state().composer_pointer)?
}

pub fn publish_feedback_header(path_key: u64, overlap: bool) {
    if feedback_ui_enabled() {
        feedback_state().headers.insert(path_key, overlap);
    }
}

pub fn feedback_header_snapshot() -> FeedbackHeaderSnapshot {
    if !feedback_ui_enabled() {
        return FeedbackHeaderSnapshot::default();
    }
    let state = feedback_state();
    FeedbackHeaderSnapshot {
        observations: state.headers.len(),
        overlap: state.headers.values().any(|overlap| *overlap),
    }
}

pub fn publish_feedback_sound_slider(bounds: FeedbackRect, value: f32, enabled: bool) {
    if !feedback_ui_enabled() {
        return;
    }
    let mut state = feedback_state();
    let previous = state.sound_slider.unwrap_or_default();
    state.sound_slider = Some(FeedbackSoundSliderSnapshot {
        bounds,
        value,
        enabled,
        hovered: previous.hovered,
        clicked_value: previous.clicked_value,
    });
}

pub fn set_feedback_sound_slider_hovered(hovered: bool) {
    if feedback_ui_enabled()
        && let Some(snapshot) = feedback_state().sound_slider.as_mut()
    {
        snapshot.hovered = hovered;
    }
}

pub fn set_feedback_sound_slider_clicked(value: f32) {
    if feedback_ui_enabled()
        && let Some(snapshot) = feedback_state().sound_slider.as_mut()
    {
        snapshot.clicked_value = Some(value);
        snapshot.value = value;
    }
}

pub fn feedback_sound_slider_snapshot() -> Option<FeedbackSoundSliderSnapshot> {
    feedback_ui_enabled().then(|| feedback_state().sound_slider)?
}

/// Persistence owners use this to keep synthetic tabs/settings off disk.
pub fn should_suppress_persistence() -> bool {
    disposable_workspace_enabled()
}

/// Cap only the synthetic tab layout. The real main-window geometry and every
/// ordinary Echo tab bar retain their measured width.
pub fn cap_tab_layout_width(width: f32) -> f32 {
    if is_enabled() {
        width.min(FIXTURE_TAB_LAYOUT_WIDTH_PX)
    } else {
        width
    }
}

/// The minimal tab appearance is the fixed, non-persisting appearance for
/// this fixture.
pub fn tab_theme_override() -> Option<TabTheme> {
    is_enabled().then_some(TabTheme::Minimal)
}

/// Return the current non-persisting overflow stage for the fixture.
pub fn tab_overflow_override() -> Option<TabOverflowMode> {
    if !is_enabled() {
        return None;
    }

    Some(match RUNTIME_OVERFLOW_MODE.load(Ordering::Acquire) {
        OVERFLOW_DROPDOWN => TabOverflowMode::Dropdown,
        OVERFLOW_MULTILINE => TabOverflowMode::MultiLine,
        _ => TabOverflowMode::Scroll,
    })
}

/// Advance the fixture without writing Echo's persisted settings.
pub fn set_tab_overflow_override(mode: TabOverflowMode) -> bool {
    if !is_enabled() {
        return false;
    }

    let encoded = match mode {
        TabOverflowMode::Scroll => OVERFLOW_SCROLL,
        TabOverflowMode::Dropdown => OVERFLOW_DROPDOWN,
        TabOverflowMode::MultiLine => OVERFLOW_MULTILINE,
    };
    RUNTIME_OVERFLOW_MODE.store(encoded, Ordering::Release);
    true
}

pub fn completion_inline_candidates(
    channel: &str,
    input: &str,
) -> Vec<crate::components::overlays::InlineTabCompletionCandidate> {
    use crate::components::overlays::{Autocomplete, AutocompleteHelper, AutocompleteState};

    if !is_enabled() {
        return Vec::new();
    }
    let Some(context) = AutocompleteHelper::detect_tab_context(input, input.len()) else {
        return Vec::new();
    };
    let mut autocomplete = Autocomplete::new(AutocompleteState::default());
    autocomplete.update_state(AutocompleteState::open(
        0.0,
        0.0,
        channel.to_string(),
        crate::state::SplitId::new(),
        context,
    ));
    autocomplete.inline_tab_candidates()
}

fn fixture_tab(title: &str, channel: &str) -> TabState {
    let mut tab = TabState::with_layout(SplitNode::leaf(channel.to_string()));
    tab.custom_title = Some(title.to_string());
    tab
}

/// Replace only the in-memory workspace with a deterministic, non-networked
/// visual fixture. Callers must invoke this before `EchoApp` starts its normal
/// automation driver and must honor [`should_suppress_persistence`].
pub fn seed_app_state(state: &mut AppState) -> Option<UiFixtureSemanticSnapshot> {
    if !disposable_workspace_enabled() {
        return None;
    }

    RUNTIME_OVERFLOW_MODE.store(OVERFLOW_SCROLL, Ordering::Release);

    let _ = crate::backend::emotes::seed_completion_emotes_for_benchmark(OFFLINE_CHANNEL);
    let _ = crate::backend::irc::clear_recent_chatters_for_benchmark(OFFLINE_CHANNEL);
    for (login, display) in COMPLETION_CHATTERS.iter().rev() {
        crate::backend::irc::track_chatter(OFFLINE_CHANNEL, login, display, None);
    }

    let header_layout = if responsive_matrix_enabled() {
        SplitNode::split(
            SplitOrientation::Horizontal,
            vec![
                SplitNode::split(
                    SplitOrientation::Vertical,
                    vec![
                        SplitNode::leaf(OFFLINE_CHANNEL.to_string()),
                        SplitNode::leaf(LIVE_CHANNEL.to_string()),
                    ],
                    vec![0.5, 0.5],
                ),
                SplitNode::split(
                    SplitOrientation::Vertical,
                    vec![
                        SplitNode::leaf(HIGHLIGHT_LIVE_CHANNEL.to_string()),
                        SplitNode::leaf("echo-unread-fixture".to_string()),
                    ],
                    vec![0.5, 0.5],
                ),
            ],
            vec![0.5, 0.5],
        )
    } else {
        SplitNode::split(
            SplitOrientation::Vertical,
            vec![
                SplitNode::leaf(OFFLINE_CHANNEL.to_string()),
                SplitNode::leaf(LIVE_CHANNEL.to_string()),
            ],
            vec![0.5, 0.5],
        )
    };
    let mut selected = TabState::with_layout(header_layout);
    selected.custom_title = Some("Header fixtures".to_string());
    selected.is_selected = true;
    selected.is_live = true;
    selected.active_split_path = if responsive_matrix_enabled() {
        vec![0, 0]
    } else {
        vec![0]
    };

    let mut highlighted = fixture_tab("Highlighted", "echo-highlighted-fixture");
    highlighted.highlight_state = TabHighlightState::Highlighted;
    highlighted.has_unread = true;

    let mut new_message = fixture_tab("New message", "echo-new-message-fixture");
    new_message.highlight_state = TabHighlightState::NewMessage;
    new_message.has_unread = true;

    let mut unread_fallback = fixture_tab("Unread fallback", "echo-unread-fixture");
    unread_fallback.has_unread = true;

    let regular = fixture_tab("Regular hover", "echo-regular-fixture");

    let mut highlighted_live = fixture_tab("Highlighted live", HIGHLIGHT_LIVE_CHANNEL);
    highlighted_live.highlight_state = TabHighlightState::Highlighted;
    highlighted_live.has_unread = true;
    highlighted_live.is_live = true;

    let long_unicode = fixture_tab(
        "A deliberately very long Unicode tab title — 狐狸🚀 ellipsis boundary",
        "echo-unicode-fixture",
    );
    let overflow = fixture_tab("Overflow final", "echo-overflow-fixture");

    state.tabs = Arc::new(vec![
        selected,
        highlighted,
        new_message,
        unread_fallback,
        regular,
        highlighted_live,
        long_unicode,
        overflow,
    ]);
    state.active_tab_idx = 0;

    let started_at = crate::utils::unix_now_secs().saturating_sub(3_730) as i64;
    state.stream_info = HashMap::from([
        (
            OFFLINE_CHANNEL.to_string(),
            Some(StreamInfo {
                title: "Offline set title retained".to_string(),
                game_name: Some("Creative Coding".to_string()),
                viewer_count: 0,
                started_at: 0,
                is_live: false,
            }),
        ),
        (
            LIVE_CHANNEL.to_string(),
            Some(StreamInfo {
                title: "Live header fixture".to_string(),
                game_name: Some("Software and Game Development".to_string()),
                viewer_count: 12_345,
                started_at,
                is_live: true,
            }),
        ),
        (
            HIGHLIGHT_LIVE_CHANNEL.to_string(),
            Some(StreamInfo {
                title: "Highlighted live tab fixture".to_string(),
                game_name: Some("Just Chatting".to_string()),
                viewer_count: 321,
                started_at,
                is_live: true,
            }),
        ),
    ]);

    seed_fixture_room_states();

    Some(snapshot(state))
}

fn seed_fixture_room_states() {
    let room_state = crate::backend::irc::room_state::RoomState {
        emote_only: true,
        subscribers_only: true,
        r9k: true,
        slow_mode: Some(Duration::from_secs(10)),
        followers_only: crate::backend::irc::room_state::FollowersOnly::Minimum(
            Duration::from_secs(5 * 60),
        ),
    };
    let _ = crate::backend::irc::room_state::set_room_state_for_benchmark(
        OFFLINE_CHANNEL,
        room_state.clone(),
    );
    let _ = crate::backend::irc::room_state::set_room_state_for_benchmark(LIVE_CHANNEL, room_state);
}

pub fn snapshot(state: &AppState) -> UiFixtureSemanticSnapshot {
    let offline = state
        .stream_info
        .get(OFFLINE_CHANNEL)
        .and_then(|info| info.as_ref());
    let live = state
        .stream_info
        .get(LIVE_CHANNEL)
        .and_then(|info| info.as_ref());

    let selected_tabs = state.tabs.iter().filter(|tab| tab.is_selected).count();
    let highlighted_tabs = state
        .tabs
        .iter()
        .enumerate()
        .filter(|(idx, tab)| {
            crate::components::tabs::TabBarRenderer::minimal_visual_state(
                tab,
                *idx == state.active_tab_idx,
            ) == TabHighlightState::Highlighted
        })
        .count();
    let new_message_tabs = state
        .tabs
        .iter()
        .enumerate()
        .filter(|(idx, tab)| {
            tab.highlight_state == TabHighlightState::NewMessage
                && crate::components::tabs::TabBarRenderer::minimal_visual_state(
                    tab,
                    *idx == state.active_tab_idx,
                ) == TabHighlightState::NewMessage
        })
        .count();
    let unread_fallback_tabs = state
        .tabs
        .iter()
        .enumerate()
        .filter(|(idx, tab)| {
            tab.highlight_state == TabHighlightState::None
                && tab.has_unread
                && crate::components::tabs::TabBarRenderer::minimal_visual_state(
                    tab,
                    *idx == state.active_tab_idx,
                ) == TabHighlightState::NewMessage
        })
        .count();
    let live_tabs = state
        .tabs
        .iter()
        .filter(|tab| {
            tab.channels().iter().any(|channel| {
                state
                    .stream_info
                    .get(channel)
                    .and_then(|info| info.as_ref())
                    .is_some_and(|info| info.is_live)
            })
        })
        .count();
    let long_title_tabs = state
        .tabs
        .iter()
        .filter(|tab| tab.display_title().chars().count() > 40)
        .count();

    UiFixtureSemanticSnapshot {
        tabs: state.tabs.len(),
        splits: state
            .tabs
            .first()
            .map_or(0, |tab| tab.layout.count_leaves()),
        offline_status: if offline.is_some_and(|info| info.is_live) {
            "LIVE"
        } else {
            "OFFLINE"
        },
        offline_title: offline.is_some_and(|info| !info.title.trim().is_empty()),
        offline_category: offline.is_some_and(|info| {
            info.game_name
                .as_deref()
                .is_some_and(|category| !category.trim().is_empty())
        }),
        live_status: if live.is_some_and(|info| info.is_live) {
            "LIVE"
        } else {
            "OFFLINE"
        },
        live_title: live.is_some_and(|info| !info.title.trim().is_empty()),
        live_category: live.is_some_and(|info| {
            info.game_name
                .as_deref()
                .is_some_and(|category| !category.trim().is_empty())
        }),
        live_viewers: live.map_or(0, |info| info.viewer_count),
        roomstate_labels: live_roomstate_label_count(),
        selected_tabs,
        highlighted_tabs,
        new_message_tabs,
        unread_fallback_tabs,
        live_tabs,
        long_title_tabs,
    }
}

fn roomstate_label_count(state: crate::backend::irc::room_state::RoomState) -> usize {
    usize::from(state.emote_only)
        + usize::from(state.subscribers_only)
        + usize::from(state.r9k)
        + usize::from(state.slow_mode.is_some())
        + usize::from(!matches!(
            state.followers_only,
            crate::backend::irc::room_state::FollowersOnly::Disabled
        ))
}

fn live_roomstate_label_count() -> usize {
    crate::backend::irc::room_state::get_room_state(LIVE_CHANNEL)
        .map(roomstate_label_count)
        .unwrap_or(0)
}

/// Remove only global state owned by the disposable fixture.
pub fn cleanup() {
    if !disposable_workspace_enabled() {
        return;
    }

    RUNTIME_OVERFLOW_MODE.store(OVERFLOW_SCROLL, Ordering::Release);
    reset_feedback_ui_snapshots();
    for channel in [OFFLINE_CHANNEL, LIVE_CHANNEL, HIGHLIGHT_LIVE_CHANNEL] {
        let _ = crate::backend::irc::room_state::clear_room_state(channel);
    }
    let _ = crate::backend::emotes::clear_completion_emotes_for_benchmark(OFFLINE_CHANNEL);
    let _ = crate::backend::irc::clear_recent_chatters_for_benchmark(OFFLINE_CHANNEL);
}
