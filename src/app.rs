//! Main application shell
//!
//! Contains the root view that renders the entire application UI.
#![allow(clippy::too_many_arguments)]

mod benchmark_drivers;
mod chatterino_import;

use crate::backend;
use crate::backend::lock_utils::MutexExt;
use crate::components::auth::LoginModal;
use crate::components::chat::chat_header_renderer::main_split_header_height;
use crate::components::chat::chat_pane::{
    SplitLeafParamsGate, SplitLeafRenderParams, SplitLeafView, SplitLeafViewHandles,
    reply_state_gate_hash, stream_info_gate_hash,
};
use crate::components::chat::{ChatHeaderRenderer, ChatInputRenderer, ChatViewDirtyReason};
use crate::components::settings::SettingsSectionId;
use crate::components::splits::SplitTreeRenderer;
use crate::components::tabs::TabBarRenderer;
use crate::components::ui::menu::{MenuAction, MenuHost, MenuKeyboard, menu_item, menu_panel};
use crate::components::ui::{ButtonExt, PressToClick};
use crate::config;
use crate::icons::{MenuMetrics, modal_backdrop_style};
use crate::managers::{
    OverlayManager, PollingCoordinator, SplitDragState, SplitLayoutManager, TabManager,
    refresh_auth_cache,
};
use crate::state::{
    ActivitySection, AddSplitDirection, AppState, DropZone, SplitId, SplitNode, SplitOrientation,
    is_placeholder,
};
use crate::theme::colors;
use crate::theme::tokens::{Radii, RadiiPixels, Spacing};
use gpui::prelude::FluentBuilder;
use gpui::*;
#[cfg(target_os = "windows")]
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use rustc_hash::FxHasher;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::rc::Rc;
use std::time::{Duration, Instant};
use unicode_segmentation::UnicodeSegmentation;
#[cfg(target_os = "windows")]
use windows::Win32::Foundation::HWND;
#[cfg(target_os = "windows")]
use windows::Win32::UI::WindowsAndMessaging::{
    HWND_NOTOPMOST, HWND_TOPMOST, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SetWindowPos,
};

const PENDING_UI_FAST_INTERVAL: Duration = Duration::from_millis(50);
const PENDING_UI_EMBED_INTERVAL: Duration = Duration::from_millis(100);
const PENDING_UI_REAUTH_INTERVAL: Duration = Duration::from_millis(250);
const PENDING_UI_SETTINGS_INTERVAL: Duration = Duration::from_millis(250);
const PENDING_UI_ORPHAN_PRUNE_INTERVAL: Duration = Duration::from_secs(1);
/// How often a visible Live now list asks whether its data has expired. The
/// list's own TTL decides whether that turns into a Helix request.
const PENDING_UI_FOLLOWED_LIVE_INTERVAL: Duration = Duration::from_secs(1);

pub(crate) struct TabCloseRequest {
    pub tab_id: String,
}

#[derive(Clone, Copy)]
enum TabCloseScope {
    Single,
    Others,
    ToRight,
}

/// Read-only probe for `SplitNode::ensure_split_ids`, which needs an owned tree.
/// SplitIds are assigned once, so this answers "no" for the whole session after
/// the first migration pass and the deep clone never happens again.
fn layout_has_missing_split_ids(node: &SplitNode) -> bool {
    match node {
        SplitNode::Leaf { split_id, .. } => split_id.is_none(),
        SplitNode::Split {
            split_id, children, ..
        } => split_id.is_none() || children.iter().any(layout_has_missing_split_ids),
    }
}

/// Whether any leaf is an empty split showing the channel chooser.
fn layout_has_placeholder(node: &SplitNode) -> bool {
    match node {
        SplitNode::Leaf { channel, .. } => is_placeholder(channel),
        SplitNode::Split { children, .. } => children.iter().any(layout_has_placeholder),
    }
}

/// Width the split tree keeps for itself before the activity rail may claim any
/// space. Without this the fixed 320px rail subtracts the whole viewport at the
/// 300px window minimum and the chat pane renders at zero width.
const MIN_SPLIT_TREE_WIDTH_PX: f32 = 220.0;

/// Narrowest the activity rail is allowed to shrink to before it starts taking
/// space back from the split tree; below this its own header and tab row stop
/// being legible at all.
const MIN_ACTIVITY_SIDEBAR_WIDTH_PX: f32 = 180.0;

/// Below this width Activity floats over the split tree. A docked 320px rail
/// leaves a narrow window with an unusable chat canvas long before the split
/// tree reaches its own absolute minimum.
const ACTIVITY_OVERLAY_BREAKPOINT_PX: f32 = 640.0;

/// Resolve how much of `available_width` the activity rail may occupy. The rail
/// surrenders the width it cannot have so the chat pane is never erased; only
/// when the viewport cannot seat both minimums do the two split it evenly.
fn activity_sidebar_width(available_width: f32) -> f32 {
    let spare = (available_width - MIN_SPLIT_TREE_WIDTH_PX).max(0.0);
    if spare >= MIN_ACTIVITY_SIDEBAR_WIDTH_PX {
        spare.min(config::SIDEBAR_WIDTH_PX)
    } else {
        (available_width * 0.5).min(MIN_ACTIVITY_SIDEBAR_WIDTH_PX)
    }
}

/// Actions for the app
pub mod actions {
    use gpui::*;

    actions!(
        echo,
        [
            AddChannel,
            SendMessage,
            Backspace,
            OpenSettings,
            OpenSearch,
            CloseTab,
            NextTab,
            PrevTab,
            ScrollToBottom,
            FocusInput,
            HistoryUp,
            HistoryDown,
        ]
    );
}

#[cfg(target_os = "windows")]
fn main_window_hwnd(window: &Window) -> Option<HWND> {
    let handle = HasWindowHandle::window_handle(window).ok()?;
    match handle.as_raw() {
        RawWindowHandle::Win32(win32) => Some(HWND(win32.hwnd.get() as *mut core::ffi::c_void)),
        _ => None,
    }
}

#[cfg(target_os = "windows")]
pub(crate) fn apply_main_window_always_on_top(window: &Window, enabled: bool) -> bool {
    let Some(hwnd) = main_window_hwnd(window) else {
        return false;
    };

    let insert_after = if enabled {
        HWND_TOPMOST
    } else {
        HWND_NOTOPMOST
    };

    // SAFETY: `hwnd` is this process's live main window handle, and SetWindowPos only changes its z-order flags.
    unsafe {
        SetWindowPos(
            hwnd,
            Some(insert_after),
            0,
            0,
            0,
            0,
            SWP_NOACTIVATE | SWP_NOMOVE | SWP_NOSIZE,
        )
        .is_ok()
    }
}

/// Neither Wayland nor GPUI can keep a window above others; KWin can.
#[cfg(target_os = "linux")]
pub(crate) fn apply_main_window_always_on_top(_window: &Window, enabled: bool) -> bool {
    crate::utils::kwin::set_main_window_keep_above(enabled);
    true
}

#[cfg(not(any(target_os = "windows", target_os = "linux")))]
pub(crate) fn apply_main_window_always_on_top(_window: &Window, _enabled: bool) -> bool {
    false
}

fn find_first_path_for_channel(root: &SplitNode, target_channel: &str) -> Option<Vec<usize>> {
    fn walk(node: &SplitNode, target_channel: &str, path: &mut Vec<usize>) -> Option<Vec<usize>> {
        match node {
            SplitNode::Leaf { channel, .. } => {
                if channel.eq_ignore_ascii_case(target_channel) {
                    Some(path.clone())
                } else {
                    None
                }
            }
            SplitNode::Split { children, .. } => {
                for (idx, child) in children.iter().enumerate() {
                    path.push(idx);
                    if let Some(found) = walk(child, target_channel, path) {
                        return Some(found);
                    }
                    path.pop();
                }
                None
            }
        }
    }

    walk(root, target_channel, &mut Vec::new())
}

/// A split showing `channel` in `tab`: the tab's active split when it shows
/// the channel, so a channel open in several splits targets the one in use,
/// otherwise the first such split.
fn tab_channel_target(tab: &crate::state::TabState, channel: &str) -> Option<(Vec<usize>, String)> {
    let active_path = tab.valid_active_split_path();
    let path = tab
        .layout
        .get_channel_at_path(&active_path)
        .filter(|active_channel| active_channel.eq_ignore_ascii_case(channel))
        .map(|_| active_path)
        .or_else(|| find_first_path_for_channel(&tab.layout, channel))?;
    let resolved_channel = tab
        .layout
        .get_channel_at_path(&path)
        .unwrap_or_else(|| channel.to_string());
    Some((path, resolved_channel))
}

pub(crate) fn find_open_tab_channel_target(
    app_state: &AppState,
    channel: &str,
) -> Option<(usize, Vec<usize>, String)> {
    let active_idx = app_state.active_tab_idx;
    std::iter::once((active_idx, app_state.active_tab()))
        .chain(
            app_state
                .tabs
                .iter()
                .enumerate()
                .filter(|(idx, _)| *idx != active_idx),
        )
        .find_map(|(tab_index, tab)| {
            tab_channel_target(tab, channel).map(|(path, resolved)| (tab_index, path, resolved))
        })
}

fn find_open_split_target(
    app_state: &AppState,
    split_id: SplitId,
    expected_channel: &str,
) -> Option<(usize, Vec<usize>, String)> {
    let expected_channel = expected_channel.trim_start_matches('#');
    app_state
        .tabs
        .iter()
        .enumerate()
        .find_map(|(tab_index, tab)| {
            let path = tab.split_path_for_id(split_id)?;
            let channel = tab.layout.get_channel_at_path(&path)?;
            (expected_channel.is_empty()
                || channel
                    .trim_start_matches('#')
                    .eq_ignore_ascii_case(expected_channel))
            .then_some((tab_index, path, channel))
        })
}

fn tab_active_split_target(
    tab: &crate::state::TabState,
) -> Option<(crate::state::SplitId, String)> {
    let path = tab.valid_active_split_path();
    let channel = tab.layout.get_channel_at_path(&path)?;
    if is_placeholder(&channel) {
        return None;
    }
    let split_id = tab.layout.get_split_id_at_path(&path)?;
    Some((split_id, channel))
}

/// Width band the split action menu may ask for before the viewport clamps it.
///
/// The floor keeps a short menu from reading as a tooltip; the cap stops a
/// pathological label or rebound hotkey from producing a full-width panel.
const SPLIT_MENU_MIN_WIDTH_PX: f32 = 160.0;
const SPLIT_MENU_MAX_WIDTH_PX: f32 = 320.0;
const SPLIT_MENU_MAX_HEIGHT_PX: f32 = 420.0;
/// Label column below which the shortcut column is dropped instead of elided.
const SPLIT_MENU_MIN_LABEL_PX: f32 = 72.0;
/// The panel draws a 1px border on each side, inside its requested width.
const SPLIT_MENU_BORDER_PX: f32 = 2.0;

/// Split action menu entries, kept as data so one list drives both the width
/// measurement and the painted rows.
#[derive(Clone, Copy)]
enum SplitMenuAction {
    ToggleHeader,
    AddSplit,
    ChangeChannel,
    LiveFollowedChannels,
    OpenChatters,
    OpenChannelPopup,
    ModLog,
    CycleDeletedMode,
    OpenCustomPlayer,
    OpenStreamlink,
    CloseSplit,
}

/// Short name of a deleted-message mode in the split menu.
fn deleted_mode_label(mode: backend::settings::DeletedMessageMode) -> &'static str {
    use backend::settings::DeletedMessageMode;
    match mode {
        DeletedMessageMode::Disable => "Show",
        DeletedMessageMode::Hide => "Hide",
        DeletedMessageMode::Replace => "Click to reveal",
        DeletedMessageMode::Strikethrough => "Strikethrough",
        DeletedMessageMode::Dim => "Dimmed",
    }
}

/// The mode after `current` when cycling a split's deleted-message override:
/// follow the global setting, then each explicit mode, then back.
fn next_deleted_mode(
    current: Option<backend::settings::DeletedMessageMode>,
) -> Option<backend::settings::DeletedMessageMode> {
    use backend::settings::DeletedMessageMode;
    match current {
        None => Some(DeletedMessageMode::Dim),
        Some(DeletedMessageMode::Dim) => Some(DeletedMessageMode::Strikethrough),
        Some(DeletedMessageMode::Strikethrough) => Some(DeletedMessageMode::Replace),
        Some(DeletedMessageMode::Replace) => Some(DeletedMessageMode::Hide),
        Some(DeletedMessageMode::Hide) => Some(DeletedMessageMode::Disable),
        Some(DeletedMessageMode::Disable) => None,
    }
}

/// A split action menu row: an elidable label plus an optional trailing
/// shortcut.
struct SplitMenuRow {
    action: SplitMenuAction,
    label: SharedString,
    shortcut: Option<SharedString>,
}

/// Width the split action menu needs, measured rather than guessed.
///
/// Returns the intrinsic width — widest `label + gap + shortcut` row plus the
/// shared [`MenuMetrics`] inset and the panel border — and the width the
/// shortcut column occupies, so a squeezed panel can drop that column instead
/// of clipping it. Shaping through the same `shape_line` path the tab bar uses
/// means the number matches what actually gets painted.
fn measure_split_menu_width(rows: &[SplitMenuRow], window: &Window) -> (f32, f32) {
    let font = window.text_style().font();
    let measure = |text: &SharedString, font_size: f32| -> f32 {
        if text.is_empty() {
            return 0.0;
        }
        let runs = [TextRun {
            len: text.len(),
            font: font.clone(),
            color: colors::text_primary(),
            background_color: None,
            underline: None,
            strikethrough: None,
        }];
        window
            .text_system()
            .shape_line(text.clone(), px(font_size), &runs, None)
            .width
            .as_f32()
    };

    let mut widest_row: f32 = 0.0;
    let mut widest_shortcut: f32 = 0.0;
    for row in rows {
        let label = measure(&row.label, MenuMetrics::ITEM_TEXT);
        let shortcut = row
            .shortcut
            .as_ref()
            .map(|shortcut| measure(shortcut, MenuMetrics::SECONDARY_TEXT))
            .unwrap_or(0.0);
        widest_shortcut = widest_shortcut.max(shortcut);
        widest_row = widest_row.max(if shortcut > 0.0 {
            label + MenuMetrics::ITEM_GAP + shortcut
        } else {
            label
        });
    }

    let shortcut_column = if widest_shortcut > 0.0 {
        MenuMetrics::ITEM_GAP + widest_shortcut
    } else {
        0.0
    };
    let insets = MenuMetrics::ITEM_PADDING_X * 2.0 + SPLIT_MENU_BORDER_PX;
    (widest_row.ceil() + insets, shortcut_column)
}

fn is_automod_notification_event(event: &backend::TwitchEvent) -> bool {
    matches!(
        &event.event_type,
        backend::types::TwitchEventType::AutomodMessageHold
            | backend::types::TwitchEventType::AutomodMessageUpdate
    )
}

fn pending_automod_message_id(event: &backend::TwitchEvent) -> Option<String> {
    if !matches!(
        &event.event_type,
        backend::types::TwitchEventType::AutomodMessageHold
    ) {
        return None;
    }

    event
        .message_id
        .as_deref()
        .map(str::trim)
        .filter(|message_id| !message_id.is_empty())
        .map(str::to_string)
}

fn suspicious_user_notification_target(
    event: &backend::TwitchEvent,
) -> Option<(String, String, Option<String>, String)> {
    if !matches!(
        &event.event_type,
        backend::types::TwitchEventType::SuspiciousUserMessage
            | backend::types::TwitchEventType::SuspiciousUserUpdate
    ) {
        return None;
    }

    let channel = event.channel.trim().trim_start_matches('#').to_lowercase();
    if channel.is_empty() {
        return None;
    }

    let user_login = event
        .user_login
        .as_deref()
        .or(event.user_name.as_deref())
        .unwrap_or_default()
        .trim()
        .trim_start_matches('@')
        .to_lowercase();
    let user_id = event
        .user_id
        .as_deref()
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .map(str::to_string);
    if user_login.is_empty() && user_id.is_none() {
        return None;
    }

    Some((
        channel,
        user_login,
        user_id,
        event
            .status
            .as_deref()
            .unwrap_or_default()
            .trim()
            .to_string(),
    ))
}

fn accent_action_button(
    id: SharedString,
    label: &'static str,
    accent: Hsla,
    on_click: impl Fn() + 'static,
) -> Stateful<Div> {
    div()
        .id(id)
        .px_2()
        .py_0p5()
        .rounded(Radii::sm())
        .border_1()
        .border_color(accent.opacity(0.45))
        .bg(accent.opacity(0.12))
        .cursor_pointer()
        .hover(move |s| s.bg(accent.opacity(0.2)))
        .on_activate(move |_event, _window, _cx| on_click())
        .child(
            div()
                .text_xs()
                .font_weight(FontWeight::MEDIUM)
                .text_color(accent)
                .child(label),
        )
}

fn automod_action_button(
    label: &'static str,
    message_id: String,
    allow: bool,
    event_timestamp: i64,
) -> Stateful<Div> {
    let accent = if allow {
        colors::success()
    } else {
        colors::error()
    };

    let id = SharedString::from(format!("automod-{label}-{message_id}"));
    accent_action_button(id, label, accent, move || {
        backend::manage_held_automod_message_async(message_id.clone(), allow, event_timestamp);
    })
}

fn suspicious_user_action_button(
    label: &'static str,
    channel: String,
    user_login: String,
    user_id: Option<String>,
    action: backend::commands::helix::SuspiciousUserAction,
    event_timestamp: i64,
) -> Stateful<Div> {
    let accent = match action {
        backend::commands::helix::SuspiciousUserAction::Monitor => colors::info(),
        backend::commands::helix::SuspiciousUserAction::Restrict => colors::warning(),
        backend::commands::helix::SuspiciousUserAction::Clear => colors::text_muted(),
    };

    let id = SharedString::from(format!("suspicious-{label}-{user_login}-{event_timestamp}"));
    accent_action_button(id, label, accent, move || {
        backend::manage_suspicious_user_status_async(
            channel.clone(),
            user_login.clone(),
            user_id.clone(),
            action,
            event_timestamp,
        );
    })
}

/// The root application view
pub struct EchoApp {
    /// Global application state
    pub app_state: Entity<AppState>,
    /// Focus handle for keyboard input
    focus_handle: FocusHandle,
    /// Focus handle for the shared input in unified input mode
    pub input_focus: FocusHandle,
    /// Chat input renderer entity (handles input area rendering and state)
    pub chat_input_renderer: Entity<ChatInputRenderer>,
    /// Polling coordinator entity (manages background polling loop)
    _polling_coordinator: Entity<PollingCoordinator>,
    /// Tab bar renderer entity (handles tab bar rendering and state)
    pub tab_bar_renderer: Entity<TabBarRenderer>,
    /// Chat header renderer entity (handles unified header rendering)
    pub chat_header_renderer: Entity<ChatHeaderRenderer>,
    /// Split layout manager (handles split tree state, resize, pane drag-drop)
    pub split_layout: Entity<SplitLayoutManager>,
    /// Split tree renderer entity (handles split rendering helpers)
    split_tree_renderer: Entity<SplitTreeRenderer>,
    /// Overlay manager (handles all modals, overlays, dropdowns, toasts)
    pub overlay_manager: Entity<OverlayManager>,
    /// Emote state manager (tracks emote loading state for reactive updates)
    /// All splits observe this to stay in sync with loaded emotes
    pub _emote_state: Entity<crate::state::EmoteStateManager>,
    /// App-owned image cache lifecycle/metrics facade.
    pub _image_cache_service: Entity<crate::cache::ImageCacheService>,
    /// Titlebar view, cached between frames that change none of its inputs.
    top_bar_view: Entity<crate::components::top_bar::TopBarView>,
    /// Last observed window active state (for transition-based focus updates)
    last_window_active: bool,
    /// Last raw modifier/policy tuple propagated into retained chat canvases.
    /// This keeps the app render boundary allocation-free in steady state.
    last_chat_pause_modifier_state: Option<(
        bool,
        bool,
        bool,
        bool,
        crate::backend::settings::ChatPauseModifier,
    )>,
    /// Last platform window title applied for mention-title updates.
    last_window_title: String,
    /// Last main window size persisted to settings.
    last_persisted_window_size: (f32, f32),
    /// Last main window maximized state persisted to settings.
    last_persisted_window_maximized: bool,
    /// Last main window position persisted to settings.
    last_persisted_window_position: Option<(f32, f32)>,
    /// Chat view entities keyed by stable SplitId so tab/split moves retain local render state.
    chat_views: HashMap<SplitId, Entity<crate::components::chat::ChatView>>,
    /// Last viewport/unread key consumed by `mark_observed_activity_read`.
    activity_read_visibility_key: Option<u64>,
    /// Per-split pane entities (new isolated architecture)
    /// Keyed by SplitId for stable identity that survives layout changes
    pub split_panes:
        std::collections::HashMap<crate::state::SplitId, Entity<crate::state::SplitPane>>,
    /// Per-split pane subscriptions, dropped when the split pane is removed.
    split_pane_subscriptions: std::collections::HashMap<crate::state::SplitId, Subscription>,
    /// Retained full-leaf render boundaries keyed by stable SplitId.
    split_leaf_views: std::collections::HashMap<crate::state::SplitId, Entity<SplitLeafView>>,
    file_drop_targets: HashMap<SplitId, (String, Bounds<Pixels>)>,
    /// Drains UI queues that need EchoApp-owned entities without doing that work from render.
    _pending_ui_task: Task<()>,
    /// Optional benchmark-only active-split scroll driver.
    _scroll_smoke_task: Option<Task<()>>,
    /// Optional benchmark-only in-process live UI action driver.
    _live_ui_smoke_task: Option<Task<()>>,
    /// GPUI subscriptions (kept alive for reactive updates)
    _subscriptions: Vec<Subscription>,
    /// Most recently closed non-placeholder split available for reopen hotkeys.
    last_closed_split: Option<ClosedSplitHotkeyState>,
    /// Last `AppState::closed_tab_generation` offered as an Undo toast.
    seen_closed_tab_generation: u64,
    /// Last `followed_live::version` pushed to the open Live now lists.
    seen_followed_live_version: u64,
    /// Close-action generations whose Undo toast button was pressed.
    undo_closed_tab_requests: std::sync::Arc<std::sync::Mutex<Vec<u64>>>,
    /// Last main-window transparency state applied to the live window.
    last_window_transparent: Option<bool>,
    /// Last main-window always-on-top state applied to the live window.
    last_window_always_on_top: Option<bool>,
    /// Whether startup focus has been applied through the window lifecycle path.
    initial_focus_applied: bool,
    /// One-shot focus request for a newly-created tab. The tab/split fence
    /// prevents a late mount from stealing focus after the user navigates.
    pending_new_tab_focus: Option<(usize, SplitId)>,
    /// Keyboard focus follows the active split after tab/split/overlay changes.
    pending_active_input_focus: bool,
    /// Whether a focus-taking overlay (thread viewer, quick switcher, chatter
    /// list) was open at the last housekeeping pass, so a close from any path
    /// hands focus back to the active input.
    last_focus_overlay_open: bool,
    /// Whether a chrome menu (tab, split or account menu) held keyboard focus
    /// at the last overlay render.
    last_chrome_menu_open: bool,
    last_confirmation_open: bool,
    /// Last inactive tab index prewarmed from hover state.
    last_hover_prewarm_tab_idx: Option<usize>,
    /// Stable ID of the tab whose retained chat views were last marked visible.
    last_rendered_tab_id: Option<String>,
    /// Layout content key reconciled by the last `ensure_split_panes_for_layout`.
    last_split_reconcile_key: Option<u64>,
    /// Chatterino folder offered by the first-run import card while it shows.
    chatterino_welcome: Option<PathBuf>,
}

#[derive(Clone)]
struct SplitRenderContext {
    is_dragging: bool,
    add_split_mode: bool,
    resize_mode_active: bool,
    drop_target: Option<(
        crate::managers::split_layout_manager::PathKey,
        crate::state::DropZone,
    )>,
    /// Split under the pointer. The Ctrl+Alt overlay shows only on the hovered
    /// split, never on all of them at once.
    hovered: Option<crate::managers::split_layout_manager::PathKey>,
}

struct SplitVerticalChrome {
    header: f32,
    input: f32,
    search: f32,
    pinned: f32,
}

impl SplitVerticalChrome {
    fn height(&self) -> f32 {
        self.header + self.input + self.search + self.pinned
    }
}

#[derive(Clone)]
struct ClosedSplitHotkeyState {
    channel: String,
    tab_index: usize,
    restore_anchor: Option<ClosedSplitRestoreAnchor>,
}

#[derive(Clone, Copy)]
struct ClosedSplitRestoreAnchor {
    split_id: crate::state::SplitId,
    direction: AddSplitDirection,
}

/// One coherent description of the split the unified composer targets.
///
/// Built once per render pass by [`EchoApp::unified_input_target`]. Height
/// reservation and the composer itself both consume this same value, so the
/// `#channel` target label, reply bar and room-state rail always agree.
#[derive(Clone)]
struct UnifiedInputTarget {
    channel: String,
    path: Vec<usize>,
    split_id: SplitId,
    reply_state: Option<crate::state::ReplyState>,
    room_state: Option<crate::backend::irc::room_state::RoomState>,
    pane_width: f32,
    metrics: crate::components::chat::chat_input_renderer::InputSurfaceMetrics,
}

impl EchoApp {
    pub fn new(app_state: Entity<AppState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus_handle = cx.focus_handle();
        let input_focus = cx.focus_handle();
        let main_window = gpui::Window::window_handle(window);
        app_state.update(cx, |state, _| state.main_window = Some(main_window));

        Self::init_backend();

        let emote_state = cx.new(|_cx| crate::state::EmoteStateManager::new());
        let image_cache_service = cx.new(crate::cache::ImageCacheService::new);

        let overlay_manager = cx.new(OverlayManager::new);

        let polling_coordinator = cx.new(|cx| {
            let mut coordinator =
                PollingCoordinator::new(app_state.clone(), overlay_manager.clone());
            coordinator.set_emote_state(emote_state.clone());
            coordinator.start(cx);
            coordinator
        });

        Self::setup_demo_mode(&app_state, &overlay_manager, cx);
        Self::show_startup_reliability_notices(&overlay_manager, cx);

        app_state.update(cx, |state, _| {
            let mut seen = std::collections::HashSet::new();
            let mut repaired = false;
            for tab in state.tabs_mut() {
                repaired |=
                    std::sync::Arc::make_mut(&mut tab.layout).ensure_unique_split_ids(&mut seen);
            }
            if repaired && !crate::benchmark_ui_fixture::should_suppress_persistence() {
                config::queue_save_tabs(state.tabs_for_persistence());
            }
        });

        let tab_bar_renderer = cx.new(|cx| TabBarRenderer::new(app_state.clone(), cx));
        overlay_manager.update(cx, |overlays, _| {
            overlays.attach_tab_bar(tab_bar_renderer.downgrade());
        });
        let chat_header_renderer = cx.new(ChatHeaderRenderer::new);
        let chat_input_renderer = cx.new(ChatInputRenderer::new);
        let split_tree_renderer = cx.new(|_cx| SplitTreeRenderer::new());

        let mut subscriptions = Self::create_subscriptions(&app_state, &emote_state, window, cx);
        subscriptions.push(cx.subscribe(
            &tab_bar_renderer,
            |app, _, _: &crate::components::tabs::TabBarActiveSplitChanged, cx| {
                app.refocus_active_input(cx);
            },
        ));
        subscriptions.push(cx.subscribe(
            &tab_bar_renderer,
            |app, _, request: &TabCloseRequest, cx| {
                app.close_tabs_by_id(&request.tab_id, TabCloseScope::Single, cx);
            },
        ));
        let emote_picker = overlay_manager.read(cx).emote_picker.clone();
        subscriptions.push(cx.subscribe(
            &emote_picker,
            |app, _, event: &crate::components::overlays::EmotePickerEvent, cx| match event {
                crate::components::overlays::EmotePickerEvent::PendingActions => {
                    app.process_picker_actions(cx);
                    cx.notify();
                }
                crate::components::overlays::EmotePickerEvent::PointerPressedOutside(position) => {
                    app.dismiss_emote_picker_outside(*position, cx);
                }
                crate::components::overlays::EmotePickerEvent::PopoutRequested {
                    channel,
                    split_id,
                } => {
                    crate::components::overlays::open_emote_popout_for_split(
                        channel.clone(),
                        Some(*split_id),
                        cx,
                    );
                }
            },
        ));
        let popout_events = crate::components::overlays::popout_message_events(cx);
        subscriptions.push(cx.subscribe(&popout_events, |app, _, _: &(), cx| {
            if app.process_picker_actions(cx) {
                cx.notify();
            }
        }));
        let pending_ui_task = Self::spawn_pending_ui_task(cx);
        let scroll_smoke_task = Self::spawn_scroll_smoke_task(cx);
        let live_ui_smoke_task =
            Self::spawn_live_ui_smoke_task(gpui::Window::window_handle(window), cx);
        let initial_active_split_path = app_state.read(cx).active_tab().valid_active_split_path();
        let split_layout = cx.new(move |_| {
            let mut layout = SplitLayoutManager::new();
            layout.set_active_split(initial_active_split_path);
            layout
        });
        let top_bar_view = {
            let app = cx.entity().downgrade();
            let app_state = app_state.clone();
            let overlay_manager = overlay_manager.clone();
            let split_layout = split_layout.clone();
            let tab_bar_renderer = tab_bar_renderer.clone();
            cx.new(move |_| {
                crate::components::top_bar::TopBarView::new(
                    app,
                    app_state,
                    overlay_manager,
                    split_layout,
                    tab_bar_renderer,
                )
            })
        };

        Self {
            app_state,
            focus_handle,
            input_focus,
            chat_input_renderer,
            _polling_coordinator: polling_coordinator,
            tab_bar_renderer,
            chat_header_renderer,
            split_layout,
            split_tree_renderer,
            overlay_manager,
            _emote_state: emote_state,
            _image_cache_service: image_cache_service,
            top_bar_view,
            last_window_active: true,
            last_chat_pause_modifier_state: None,
            last_window_title: "Echo".to_string(),
            last_persisted_window_size: backend::settings::get_main_window_size(),
            last_persisted_window_maximized: backend::settings::get_main_window_maximized(),
            last_persisted_window_position: backend::settings::get_main_window_position(),
            chat_views: HashMap::new(),
            activity_read_visibility_key: None,
            split_panes: std::collections::HashMap::new(),
            split_pane_subscriptions: std::collections::HashMap::new(),
            split_leaf_views: std::collections::HashMap::new(),
            file_drop_targets: HashMap::new(),
            _pending_ui_task: pending_ui_task,
            _scroll_smoke_task: scroll_smoke_task,
            _live_ui_smoke_task: live_ui_smoke_task,
            _subscriptions: subscriptions,
            last_closed_split: None,
            seen_closed_tab_generation: 0,
            seen_followed_live_version: 0,
            undo_closed_tab_requests: Default::default(),
            last_window_transparent: None,
            last_window_always_on_top: None,
            initial_focus_applied: false,
            pending_new_tab_focus: None,
            pending_active_input_focus: false,
            last_focus_overlay_open: false,
            last_chrome_menu_open: false,
            last_confirmation_open: false,
            last_hover_prewarm_tab_idx: None,
            last_rendered_tab_id: None,
            last_split_reconcile_key: None,
            chatterino_welcome: backend::chatterino_import::first_run_prompt_dir(),
        }
    }

    fn spawn_pending_ui_task(cx: &mut Context<Self>) -> Task<()> {
        cx.spawn(async move |this, cx| {
            let mut last_embed_check =
                echo_core::instant_ago(PENDING_UI_EMBED_INTERVAL).unwrap_or_else(Instant::now);
            let mut last_reauth_check =
                echo_core::instant_ago(PENDING_UI_REAUTH_INTERVAL).unwrap_or_else(Instant::now);
            let mut last_settings_sync =
                echo_core::instant_ago(PENDING_UI_SETTINGS_INTERVAL).unwrap_or_else(Instant::now);
            let mut last_orphan_prune = echo_core::instant_ago(PENDING_UI_ORPHAN_PRUNE_INTERVAL)
                .unwrap_or_else(Instant::now);
            let mut last_followed_live_check =
                echo_core::instant_ago(PENDING_UI_FOLLOWED_LIVE_INTERVAL)
                    .unwrap_or_else(Instant::now);

            loop {
                let interval = match crate::managers::frame_rate_controller::current_mode_cached() {
                    crate::managers::frame_rate_controller::RenderMode::Interactive
                    | crate::managers::frame_rate_controller::RenderMode::Animating => {
                        PENDING_UI_FAST_INTERVAL
                    }
                    crate::managers::frame_rate_controller::RenderMode::FocusedIdle => {
                        Duration::from_millis(100)
                    }
                    crate::managers::frame_rate_controller::RenderMode::DeepIdle => {
                        Duration::from_millis(250)
                    }
                    crate::managers::frame_rate_controller::RenderMode::TrueIdle => {
                        Duration::from_millis(500)
                    }
                };
                cx.background_executor().timer(interval).await;

                let now = Instant::now();
                if this
                    .update(cx, |app, cx| {
                        let mut needs_notify = false;

                        needs_notify |= app.process_render_housekeeping(cx);
                        crate::utils::apply_opened_urls();
                        needs_notify |= app.process_ui_invalidations(cx);

                        if now.duration_since(last_embed_check) >= PENDING_UI_EMBED_INTERVAL {
                            last_embed_check = now;
                            needs_notify |= app.update_embed_metadata(cx);
                        }

                        if now.duration_since(last_reauth_check) >= PENDING_UI_REAUTH_INTERVAL {
                            last_reauth_check = now;
                            needs_notify |= app.check_reauth_status(cx);
                        }

                        if now.duration_since(last_settings_sync) >= PENDING_UI_SETTINGS_INTERVAL {
                            last_settings_sync = now;
                            needs_notify |= app.process_pending_settings_sync(cx);
                        }

                        needs_notify |= app.process_pending_ui_work(cx);

                        let followed_live_due = now.duration_since(last_followed_live_check)
                            >= PENDING_UI_FOLLOWED_LIVE_INTERVAL;
                        if followed_live_due {
                            last_followed_live_check = now;
                        }
                        needs_notify |= app.sync_followed_live(followed_live_due, cx);

                        needs_notify |= app.mark_observed_activity_read(cx);

                        needs_notify |= app
                            .app_state
                            .update(cx, |state, _cx| state.update_tab_highlight_states());

                        if now.duration_since(last_orphan_prune) >= PENDING_UI_ORPHAN_PRUNE_INTERVAL
                        {
                            last_orphan_prune = now;
                            needs_notify |= app.prune_orphaned_chat_views(cx);
                        }

                        if needs_notify {
                            crate::backend::metrics::record_ui_notify_source("app.pending_ui_task");
                            cx.notify();
                        }
                    })
                    .is_err()
                {
                    break;
                }
            }
        })
    }

    /// Initialize backend systems (auth, hotkeys)
    fn init_backend() {
        refresh_auth_cache();

        backend::bridge::initialize_auth();
        backend::bridge::start_token_refresh_monitor();

        backend::hotkeys::init_hotkeys();
    }

    /// Set up demo mode if enabled
    fn setup_demo_mode(
        app_state: &Entity<AppState>,
        overlay_manager: &Entity<OverlayManager>,
        cx: &mut Context<Self>,
    ) {
        if !crate::demo::is_demo_mode() {
            return;
        }

        let demo_channel = crate::demo::get_demo_channel().to_string();
        crate::log_ui!(info, "Adding channel {} to active tab", demo_channel);
        app_state.update(cx, |state, cx| {
            state.add_channel(demo_channel.clone(), cx);
        });

        overlay_manager.update(cx, |manager, cx| {
            manager.toast_info(
                format!("Demo mode: Testing #{}", crate::demo::get_demo_channel()),
                cx,
            );
        });

        let overlay_manager_clone = overlay_manager.clone();
        cx.spawn(async move |_this, cx| {
            cx.background_executor()
                .timer(std::time::Duration::from_secs(3))
                .await;

            let results = crate::demo::run_demo_tests().await;
            let summary = results.summary();
            let is_success = results.is_success();
            let total_emotes = results.total_emotes();
            let history_count = results.history_message_count;
            let error_msg = results.errors.first().cloned().unwrap_or_default();

            cx.update(|cx| {
                overlay_manager_clone.update(cx, |manager, cx| {
                    if is_success {
                        manager.toast_success(
                            format!(
                                "Demo OK: {} emotes, {} msgs loaded",
                                total_emotes, history_count
                            ),
                            cx,
                        );
                    } else {
                        manager.toast_error(format!("Demo failed: {}", error_msg), cx);
                    }
                });
            });

            crate::log_ui!(info, "Demo results:\n{}", summary);
        })
        .detach();
    }

    fn show_startup_reliability_notices(
        overlay_manager: &Entity<OverlayManager>,
        cx: &mut Context<Self>,
    ) {
        let notices = crate::backend::reliability::drain_startup_notices();
        if notices.is_empty() {
            return;
        }

        overlay_manager.update(cx, |manager, cx| {
            for notice in notices {
                let crate::backend::reliability::ReliabilityNotice {
                    level,
                    message,
                    detail,
                } = notice;
                let toast = match level {
                    crate::backend::reliability::ReliabilityNoticeLevel::Info => {
                        crate::components::overlays::Toast::info(message)
                    }
                    crate::backend::reliability::ReliabilityNoticeLevel::Warning => {
                        crate::components::overlays::Toast::warning(message)
                    }
                };
                let toast = match detail {
                    Some(detail) => toast.with_detail(detail),
                    None => toast,
                }
                .with_duration(10);
                manager.push_toast(toast, cx);
            }
        });
    }

    /// Create all GPUI subscriptions for reactive updates
    fn create_subscriptions(
        app_state: &Entity<AppState>,
        emote_state: &Entity<crate::state::EmoteStateManager>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<Subscription> {
        let mut subscriptions = Vec::new();

        subscriptions.push(cx.observe_window_activation(window, |this, window, cx| {
            let is_active = window.is_window_active();
            if is_active != this.last_window_active {
                this.last_window_active = is_active;
                let modifiers = window.modifiers();
                this.split_layout.update(cx, |layout, _| {
                    if is_active {
                        layout.update_modifiers(modifiers.control, modifiers.alt);
                    } else {
                        layout.update_modifiers(false, false);
                    }
                });
                if !is_active {
                    this.cancel_pointer_drags(cx);
                }
                let _ = this.sync_chat_pause_modifiers(
                    modifiers.control,
                    modifiers.alt,
                    modifiers.shift,
                    modifiers.platform,
                    is_active,
                    cx,
                );
                crate::managers::frame_rate_controller::set_focused(is_active);
                this.sync_main_window_appearance(window);
                this.update_window_title(window);
                crate::state::ui_invalidation::request(
                    crate::state::ui_invalidation::UiInvalidation::WindowState,
                );
                if is_active {
                    crate::log_perf!(debug, "Window activated");
                } else {
                    crate::log_perf!(debug, "Window deactivated - liveness unchanged");
                }
                crate::backend::metrics::record_ui_notify_source("app.window_activation");
                cx.notify();
            }
        }));

        subscriptions.push(cx.observe_window_bounds(window, |this, window, cx| {
            this.update_layout_caches(window, cx);
            this.sync_main_window_appearance(window);
            this.update_window_title(window);
            crate::state::ui_invalidation::request(
                crate::state::ui_invalidation::UiInvalidation::WindowState,
            );
            crate::backend::metrics::record_ui_notify_source("app.window_bounds");
            cx.notify();
        }));

        subscriptions.push(cx.on_system_sleep(|_cx| backend::power::handle_system_sleep()));
        subscriptions.push(cx.on_system_wake(|_cx| backend::power::handle_system_wake()));

        subscriptions.push(cx.on_app_quit(|_, _| {
            config::flush_tab_saves(std::time::Duration::from_secs(2));
            async move {
                let shutdown_start = Instant::now();
                crate::log_ui!(info, "GPUI app quit - requesting backend shutdown");
                backend::shutdown_once("gpui_app_quit");
                crate::log_ui!(
                    info,
                    elapsed_ms = shutdown_start.elapsed().as_millis(),
                    "GPUI app quit shutdown path complete"
                );
            }
        }));

        let chat_backend = app_state.read(cx).chat_backend.clone();
        subscriptions.push(cx.subscribe(
            &chat_backend,
            move |_this, _entity, event: &crate::state::ChatBackendEvent, cx| {
                use crate::state::ChatBackendEvent;
                match event {
                    ChatBackendEvent::ChannelAdded { channel } => {
                        crate::log_ui!(debug, "ChatBackend: channel '{}' added", channel);
                        crate::backend::metrics::record_ui_notify_source(
                            "app.chat_backend.channel_added",
                        );
                        cx.notify();
                    }
                    ChatBackendEvent::ChannelRemoved { channel } => {
                        crate::log_ui!(debug, "ChatBackend: channel '{}' removed", channel);
                        crate::backend::metrics::record_ui_notify_source(
                            "app.chat_backend.channel_removed",
                        );
                        cx.notify();
                    }
                }
            },
        ));

        subscriptions.push(cx.observe_release(&chat_backend, |_this, _entity, _cx| {
            let shutdown_start = Instant::now();
            crate::log_ui!(info, "ChatBackend released - requesting backend shutdown");
            backend::shutdown();
            crate::log_ui!(
                info,
                elapsed_ms = shutdown_start.elapsed().as_millis(),
                "ChatBackend release shutdown path complete"
            );
        }));

        subscriptions.push(cx.subscribe(
            emote_state,
            |_this, _entity, event: &crate::state::EmoteStateEvent, cx| {
                use crate::state::EmoteStateEvent;
                match event {
                    EmoteStateEvent::GlobalEmotesLoaded { emote_count } => {
                        crate::log_emotes!(
                            info,
                            "EmoteState: global emotes loaded ({} emotes)",
                            emote_count
                        );
                        crate::backend::metrics::record_ui_notify_source(
                            "app.emotes.global_loaded",
                        );
                        cx.notify();
                    }
                    EmoteStateEvent::UserEmotesLoaded { emote_count } => {
                        crate::log_emotes!(
                            info,
                            "EmoteState: user emotes loaded ({} emotes)",
                            emote_count
                        );
                        crate::backend::metrics::record_ui_notify_source("app.emotes.user_loaded");
                        cx.notify();
                    }
                    EmoteStateEvent::ChannelEmotesLoaded {
                        channel,
                        emote_count,
                    } => {
                        crate::log_emotes!(
                            info,
                            "EmoteState: channel '{}' emotes loaded ({} emotes)",
                            channel,
                            emote_count
                        );
                        crate::backend::metrics::record_ui_notify_source(
                            "app.emotes.channel_loaded",
                        );
                        cx.notify();
                    }
                }
            },
        ));

        subscriptions
    }

    #[inline]
    pub fn autocomplete(
        &self,
        cx: &Context<Self>,
    ) -> Entity<crate::components::overlays::Autocomplete> {
        self.overlay_manager.read(cx).autocomplete.clone()
    }

    fn active_split_target(&self, cx: &Context<Self>) -> Option<(String, Vec<usize>, SplitId)> {
        let path = self.split_layout.read(cx).get_active_split_path().to_vec();
        self.split_target_for_path(&path, cx)
            .map(|(channel, split_id)| (channel, path, split_id))
    }

    /// Activate the composer captured by a pointer or accessibility action.
    ///
    /// The split id is stable while paths are not, so resolve it again against
    /// the active tab before changing focus or submitting its draft.
    pub(crate) fn activate_composer_split(
        &mut self,
        split_id: SplitId,
        channel: &str,
        cx: &mut Context<Self>,
    ) -> Option<Vec<usize>> {
        let path = {
            let app_state = self.app_state.read(cx);
            let tab = app_state.active_tab();
            let path = tab.split_path_for_id(split_id)?;
            let stored_channel = tab.layout.get_channel_at_path(&path)?;
            let current_channel =
                backend::browser_watching::resolve_layout_channel(&stored_channel)?;
            current_channel
                .trim_start_matches('#')
                .eq_ignore_ascii_case(channel.trim_start_matches('#'))
                .then_some(path)
        }?;
        self.set_active_split(path.clone(), cx);
        Some(path)
    }

    fn split_target_for_path(
        &self,
        path: &[usize],
        cx: &Context<Self>,
    ) -> Option<(String, SplitId)> {
        let app_state = self.app_state.read(cx);
        let active_tab = app_state.active_tab();
        let stored_channel = active_tab.layout.get_channel_at_path(path)?;
        let channel = crate::backend::browser_watching::resolve_layout_channel(&stored_channel)?;
        let split_id = active_tab.layout.get_split_id_at_path(path)?;
        Some((channel, split_id))
    }

    fn valid_leaf_split_ids(&self, cx: &Context<Self>) -> std::collections::HashSet<SplitId> {
        self.app_state
            .read(cx)
            .tabs
            .iter()
            .flat_map(|tab| tab.layout.collect_split_ids())
            .collect()
    }

    fn prewarm_hovered_tab(&mut self, cx: &mut Context<Self>) -> bool {
        let hovered_idx = self.tab_bar_renderer.read(cx).tab_manager.hovered_idx;
        let active_idx = self.app_state.read(cx).active_tab_idx;
        if hovered_idx.is_none() || hovered_idx == Some(active_idx) {
            self.last_hover_prewarm_tab_idx = hovered_idx;
            return false;
        }

        if self.last_hover_prewarm_tab_idx == hovered_idx {
            return false;
        }
        self.last_hover_prewarm_tab_idx = hovered_idx;

        let Some(idx) = hovered_idx else {
            return false;
        };
        let target = {
            let app_state = self.app_state.read(cx);
            app_state.tabs.get(idx).and_then(tab_active_split_target)
        };
        let Some((split_id, channel)) = target else {
            return false;
        };

        let Some(chat_view) = self.chat_views.get(&split_id).cloned() else {
            return false;
        };
        if chat_view.read(cx).channel != channel {
            return false;
        }

        let requested = chat_view.update(cx, |view, cx| view.prewarm_for_activation(cx));
        if requested > 0 {
            crate::log_perf_render!(
                trace,
                tab_index = idx,
                split_id = %split_id,
                channel = %channel,
                requested,
                "Prewarmed hovered tab layouts"
            );
            return true;
        }

        false
    }

    /// Send the current message.
    pub fn send_message(&mut self, cx: &mut Context<Self>) {
        let Some((channel, _, split_id)) = self.active_split_target(cx) else {
            return;
        };
        self.send_message_for_split(split_id, &channel, cx);
    }

    /// Send the draft belonging to the composer that initiated the action.
    pub(crate) fn send_message_for_split(
        &mut self,
        split_id: SplitId,
        channel: &str,
        cx: &mut Context<Self>,
    ) {
        let Some(path) = self.activate_composer_split(split_id, channel, cx) else {
            return;
        };
        let Some((channel, resolved_split_id)) = self.split_target_for_path(&path, cx) else {
            return;
        };
        if resolved_split_id != split_id {
            return;
        }
        if self
            .chat_input_renderer
            .read(cx)
            .multiline_pastes
            .contains_key(&split_id)
        {
            self.resolve_multiline_paste(
                split_id,
                None,
                crate::components::chat::chat_input_renderer::MultilinePasteAction::OneLine,
                cx,
            );
            return;
        }
        let input_text = self
            .chat_input_renderer
            .read(cx)
            .input_manager
            .text_for_split(split_id)
            .to_string();
        let reply = self
            .split_panes
            .get(&split_id)
            .and_then(|pane| pane.read(cx).reply_to.clone());
        self.submit_composer_message(channel, split_id, input_text, reply, true, cx);
    }

    fn composer_target_is_current(&self, split_id: SplitId, channel: &str, cx: &App) -> bool {
        find_open_split_target(self.app_state.read(cx), split_id, "")
            .and_then(|(_, _, stored)| backend::browser_watching::resolve_layout_channel(&stored))
            .is_some_and(|current| {
                current
                    .trim_start_matches('#')
                    .eq_ignore_ascii_case(channel.trim_start_matches('#'))
            })
    }

    fn submit_composer_message(
        &mut self,
        channel: String,
        split_id: SplitId,
        input_text: String,
        reply: Option<crate::state::ReplyState>,
        clear_draft: bool,
        cx: &mut Context<Self>,
    ) -> Option<u64> {
        if !self.composer_target_is_current(split_id, &channel, cx) {
            return None;
        }
        if input_text.trim().is_empty() {
            return None;
        }

        let action_message = input_text
            .get(..4)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("/me "));
        let sends_chat_payload = !input_text.starts_with('/')
            || action_message
            || !backend::settings::get_slash_commands_enabled();
        if sends_chat_payload && backend::whispers::is_whispers_channel(&channel) {
            let toast = crate::components::overlays::Toast::warning(
                "Choose a whisper recipient",
            )
            .with_detail("Use /w <user> <message>, /r <message>, or a Reply action. Your draft was kept.");
            self.overlay_manager.update(cx, |manager, cx| {
                manager.push_toast(toast, cx);
            });
            cx.notify();
            return None;
        }
        if sends_chat_payload {
            let input_char_count = input_text.graphemes(true).count();
            if input_char_count > config::TWITCH_MESSAGE_CHAR_LIMIT {
                self.chat_input_renderer.update(cx, |renderer, cx| {
                    renderer.set_send_notice(
                        split_id,
                        &channel,
                        format!(
                            "Message too long — {input_char_count}/{} characters",
                            config::TWITCH_MESSAGE_CHAR_LIMIT
                        ),
                        cx,
                    )
                });
                self.notify_split_leaf(split_id, cx);
                let toast = crate::components::overlays::Toast::warning("Message too long")
                    .with_detail(format!(
                        "{} / {} characters",
                        input_char_count,
                        config::TWITCH_MESSAGE_CHAR_LIMIT
                    ));
                self.overlay_manager.update(cx, |manager, cx| {
                    manager.push_toast(toast, cx);
                });
                cx.notify();
                return None;
            }

            if let Err(error) = backend::irc::ensure_authenticated_send_ready()
                .and_then(|_| backend::irc::ensure_channel_send_ready(&channel).map(|_| ()))
            {
                self.chat_input_renderer.update(cx, |renderer, cx| {
                    renderer.set_send_notice(split_id, &channel, error.clone(), cx)
                });
                self.notify_split_leaf(split_id, cx);
                let toast = crate::components::overlays::Toast::warning("Message not sent")
                    .with_detail(error.trim().to_string());
                self.overlay_manager.update(cx, |manager, cx| {
                    manager.push_toast(toast, cx);
                });
                cx.notify();
                return None;
            }
        }

        let submission_id = self.chat_input_renderer.update(cx, |renderer, cx| {
            renderer.begin_send(split_id, &channel, cx)
        });
        self.notify_split_leaf(split_id, cx);
        let submission_id = submission_id?;
        self.chat_input_renderer.update(cx, |renderer, _cx| {
            let channel_history = renderer
                .input_manager
                .input_history
                .entry(channel.clone())
                .or_default();
            if channel_history.is_empty() || channel_history.last() != Some(&input_text) {
                if channel_history.len() >= config::MAX_MESSAGE_HISTORY {
                    channel_history.remove(0);
                }
                channel_history.push(input_text.clone());
            }
            if clear_draft {
                renderer.input_manager.clear_draft_for_split(split_id);
                renderer.reset_composer_affinity(split_id);
            }
        });

        let origin = backend::ui_actions::SendDraftOrigin {
            submission_id,
            account: backend::ui_actions::composer_account(),
            split_id,
            text: input_text.clone(),
            reply: reply.clone(),
        };
        if input_text.starts_with('/') && !action_message {
            backend::bridge::send_message_or_command_from_draft_async(
                channel.clone(),
                input_text.clone(),
                Some(origin),
            );
            if let Some(pane) = self.split_panes.get(&split_id)
                && clear_draft
            {
                pane.update(cx, |p, _| p.clear_reply());
            }
        } else if let Some(reply) = reply {
            backend::bridge::send_reply_from_draft_async(
                channel.clone(),
                input_text.clone(),
                reply.message_id.clone(),
                reply.username.clone(),
                Some(origin),
            );
            if let Some(pane) = self.split_panes.get(&split_id)
                && clear_draft
            {
                pane.update(cx, |p, _| p.clear_reply());
            }
        } else {
            backend::bridge::send_message_from_draft_async(
                channel.clone(),
                input_text.clone(),
                Some(origin),
            );
        }

        cx.notify();
        Some(submission_id)
    }

    pub(crate) fn resolve_multiline_paste(
        &mut self,
        split_id: SplitId,
        expected_id: Option<u64>,
        action: crate::components::chat::chat_input_renderer::MultilinePasteAction,
        cx: &mut Context<Self>,
    ) {
        use crate::components::chat::chat_input_renderer::{MultilinePasteAction, MultilineSend};
        let choice = self.chat_input_renderer.update(cx, |renderer, cx| {
            if expected_id.is_some_and(|id| {
                renderer
                    .multiline_pastes
                    .get(&split_id)
                    .is_none_or(|choice| choice.id != id)
            }) {
                return None;
            }
            let choice = renderer.multiline_pastes.remove(&split_id);
            cx.notify();
            choice
        });
        self.notify_split_leaf(split_id, cx);
        let Some(choice) = choice else {
            return;
        };
        if matches!(action, MultilinePasteAction::Cancel) {
            return;
        }
        if !self.composer_target_is_current(split_id, &choice.channel, cx)
            || choice.account != backend::ui_actions::composer_account()
        {
            return;
        }
        match action {
            MultilinePasteAction::OneLine => {
                let applied = self.chat_input_renderer.update(cx, |renderer, cx| {
                    if renderer.input_manager.text_for_split(split_id) != choice.draft
                        || renderer.input_manager.cursor_for_split(split_id) != choice.cursor
                        || renderer.input_manager.selection_for_split(split_id) != choice.selection
                    {
                        renderer.set_send_notice(
                            split_id,
                            &choice.channel,
                            "Draft changed — paste again at the intended position".to_string(),
                            cx,
                        );
                        return false;
                    }
                    renderer.insert_composer_paste(split_id, &choice.channel, &choice.raw, cx)
                });
                if applied && let Some((channel, path, _)) = self.active_split_target(cx) {
                    self.update_autocomplete(&channel, &path, None, cx);
                }
            }
            MultilinePasteAction::SendLines => {
                if let Some((index, length)) = choice
                    .lines
                    .iter()
                    .enumerate()
                    .map(|(index, line)| (index, line.graphemes(true).count()))
                    .find(|(_, length)| *length > config::TWITCH_MESSAGE_CHAR_LIMIT)
                {
                    self.chat_input_renderer.update(cx, |renderer, cx| {
                        renderer.set_send_notice(
                            split_id,
                            &choice.channel,
                            format!(
                                "Nothing sent — message {} is {length}/{} characters",
                                index + 1,
                                config::TWITCH_MESSAGE_CHAR_LIMIT
                            ),
                            cx,
                        );
                        renderer.multiline_pastes.insert(split_id, choice);
                    });
                    return;
                }
                if self
                    .chat_input_renderer
                    .read(cx)
                    .multiline_sends
                    .contains_key(&split_id)
                {
                    self.chat_input_renderer.update(cx, |renderer, cx| {
                        renderer.set_send_notice(
                            split_id,
                            &choice.channel,
                            "A pasted send is already in progress".to_string(),
                            cx,
                        )
                    });
                    return;
                }
                let reply = self
                    .split_panes
                    .get(&split_id)
                    .and_then(|pane| pane.read(cx).reply_to.clone());
                let mut batch = MultilineSend {
                    channel: choice.channel,
                    account: choice.account,
                    total: choice.lines.len(),
                    remaining: choice.lines,
                    submitted: 0,
                    in_flight: 0,
                    reply,
                };
                self.submit_next_pasted_line(split_id, &mut batch, cx);
                if batch.in_flight != 0 {
                    self.chat_input_renderer.update(cx, |renderer, _| {
                        renderer.multiline_sends.insert(split_id, batch);
                    });
                }
            }
            MultilinePasteAction::Cancel => {}
        }
        self.notify_split_leaf(split_id, cx);
        cx.notify();
    }

    fn submit_next_pasted_line(
        &mut self,
        split_id: SplitId,
        batch: &mut crate::components::chat::chat_input_renderer::MultilineSend,
        cx: &mut Context<Self>,
    ) {
        batch.in_flight = 0;
        if batch.account != backend::ui_actions::composer_account()
            || !self.composer_target_is_current(split_id, &batch.channel, cx)
        {
            return;
        }
        let Some(line) = batch.remaining.pop_front() else {
            return;
        };
        if let Some(id) = self.submit_composer_message(
            batch.channel.clone(),
            split_id,
            line,
            batch.reply.clone(),
            false,
            cx,
        ) {
            batch.submitted += 1;
            batch.in_flight = id;
        }
    }

    fn advance_pasted_send(
        &mut self,
        origin: &backend::ui_actions::SendDraftOrigin,
        accepted: bool,
        reason: &str,
        cx: &mut Context<Self>,
    ) {
        let batch = self.chat_input_renderer.update(cx, |renderer, _| {
            if renderer
                .multiline_sends
                .get(&origin.split_id)
                .is_some_and(|batch| batch.in_flight == origin.submission_id)
            {
                renderer.multiline_sends.remove(&origin.split_id)
            } else {
                None
            }
        });
        let Some(mut batch) = batch else {
            return;
        };
        if accepted {
            self.submit_next_pasted_line(origin.split_id, &mut batch, cx);
            if batch.in_flight != 0 {
                self.chat_input_renderer.update(cx, |renderer, _| {
                    renderer.multiline_sends.insert(origin.split_id, batch);
                });
            }
        } else if batch.account == backend::ui_actions::composer_account()
            && self.composer_target_is_current(origin.split_id, &batch.channel, cx)
        {
            self.chat_input_renderer.update(cx, |renderer, cx| {
                renderer.set_send_notice(
                    origin.split_id,
                    &batch.channel,
                    format!("Stopped at {}/{} — {reason}", batch.submitted, batch.total),
                    cx,
                )
            });
        }
        self.notify_split_leaf(origin.split_id, cx);
        cx.notify();
    }

    pub fn send_message_keep_input(&mut self, cx: &mut Context<Self>) {
        let Some((channel, active_path, split_id)) = self.active_split_target(cx) else {
            return;
        };

        let preserved_text = self
            .chat_input_renderer
            .read(cx)
            .input_manager
            .input_texts
            .get(&split_id)
            .cloned()
            .unwrap_or_default();
        let preserved_cursor = self
            .chat_input_renderer
            .read(cx)
            .input_manager
            .cursor_for_split(split_id);
        if preserved_text.trim().is_empty() {
            return;
        }

        self.send_message(cx);

        let should_restore = !self
            .chat_input_renderer
            .read(cx)
            .input_manager
            .input_texts
            .contains_key(&split_id);
        if should_restore {
            self.chat_input_renderer.update(cx, |renderer, _| {
                renderer
                    .input_manager
                    .input_texts
                    .insert(split_id, preserved_text.clone());
                renderer
                    .input_manager
                    .set_cursor_for_split(split_id, preserved_cursor);
                renderer.reset_composer_affinity(split_id);
                renderer.input_manager.history_index.remove(&split_id);
                renderer.input_manager.temp_input.remove(&split_id);
            });
            self.update_autocomplete(&channel, &active_path, Some(&preserved_text), cx);
            cx.notify();
        }
    }

    pub fn confirm_or_upload_pasted_image(
        &mut self,
        channel: String,
        split_id: SplitId,
        width: usize,
        height: usize,
        rgba: Vec<u8>,
        source_bytes: u64,
        cx: &mut Context<Self>,
    ) {
        let config = backend::settings::get_image_uploader_config();
        if !config.enabled || !config.paste_upload_enabled {
            return;
        }

        if backend::image_uploader::should_confirm_paste_upload(&config, source_bytes) {
            let payload = std::sync::Arc::new(std::sync::Mutex::new(Some((
                channel.clone(),
                split_id,
                width,
                height,
                rgba,
            ))));
            let payload_for_confirm = payload.clone();
            let size = backend::image_uploader::format_upload_size(source_bytes);
            let (title, message) = if backend::image_uploader::should_confirm_upload_size(
                &config,
                source_bytes,
            ) {
                let threshold = backend::image_uploader::format_upload_size(
                    backend::image_uploader::large_upload_confirmation_threshold(&config),
                );
                (
                    "Upload large image?".to_string(),
                    format!(
                        "The pasted image is {}x{} and uses {} of raw clipboard data. Echo asks for confirmation at {} and above.",
                        width, height, size, threshold
                    ),
                )
            } else {
                (
                    "Upload image?".to_string(),
                    format!(
                        "The pasted {}x{} image will be uploaded to a third-party image host. You may not be able to remove it later.",
                        width, height
                    ),
                )
            };
            self.open_upload_confirmation(
                title,
                message,
                "Upload".to_string(),
                std::sync::Arc::new(move || {
                    let payload = payload_for_confirm.lock_or_recover().take();
                    if let Some((channel, split_id, width, height, rgba)) = payload {
                        crate::backend::upload_pasted_image_async(
                            channel, split_id, width, height, rgba,
                        );
                    }
                }),
                cx,
            );
        } else {
            backend::upload_pasted_image_async(channel, split_id, width, height, rgba);
        }
    }

    pub(crate) fn file_drop_target_at(
        &self,
        position: Point<Pixels>,
        cx: &App,
    ) -> Option<(String, SplitId)> {
        if self
            .overlay_manager
            .read(cx)
            .confirmation_modal
            .read(cx)
            .state
            .is_open
        {
            return None;
        }
        let tab = self.app_state.read(cx).active_tab();
        if self.last_rendered_tab_id.as_deref() != Some(tab.id.as_str()) {
            return None;
        }
        self.file_drop_targets
            .iter()
            .find_map(|(split_id, (painted_channel, bounds))| {
                if !bounds.contains(&position) {
                    return None;
                }
                let path = tab.split_path_for_id(*split_id)?;
                let stored = tab.layout.get_channel_at_path(&path)?;
                if is_placeholder(&stored) {
                    return None;
                }
                let current = backend::browser_watching::resolve_layout_channel(&stored)?;
                current
                    .eq_ignore_ascii_case(painted_channel)
                    .then(|| (painted_channel.clone(), *split_id))
            })
    }

    fn cancelled_upload_target(&mut self, cx: &mut Context<Self>) {
        self.overlay_manager.update(cx, |manager, cx| {
            manager.push_toast(
                crate::components::overlays::Toast::warning("Upload cancelled")
                    .with_detail("The destination split or account changed. Drop the files again."),
                cx,
            )
        });
        cx.notify();
    }

    pub fn confirm_or_upload_image_files(
        &mut self,
        channel: String,
        split_id: SplitId,
        paths: Vec<PathBuf>,
        source: backend::image_uploader::ImageFileSource,
        cx: &mut Context<Self>,
    ) {
        if !self.composer_target_is_current(split_id, &channel, cx) {
            self.cancelled_upload_target(cx);
            return;
        }
        let config = backend::settings::get_image_uploader_config();
        if !source.upload_enabled(&config) || paths.is_empty() {
            return;
        }

        let threshold = backend::image_uploader::large_upload_confirmation_threshold(&config);
        let mut large_count = 0usize;
        let mut known_bytes = 0u64;
        for path in &paths {
            if backend::image_uploader::mime_type_for_image_path(path).is_none() {
                continue;
            }
            if let Ok(metadata) = std::fs::metadata(path)
                && metadata.is_file()
            {
                known_bytes = known_bytes.saturating_add(metadata.len());
                if backend::image_uploader::should_confirm_upload_size(&config, metadata.len()) {
                    large_count += 1;
                }
            }
        }

        let ask_every_paste = source == backend::image_uploader::ImageFileSource::Pasted
            && backend::settings::get_ask_before_paste_upload();
        if large_count == 0 && !ask_every_paste {
            backend::upload_image_files_async(channel, split_id, paths, source);
            return;
        }

        let (title, message) = if large_count > 0 {
            let file_word = if large_count == 1 { "file" } else { "files" };
            (
                "Upload large images?".to_string(),
                format!(
                    "{} {} image {} meet or exceed the {} confirmation threshold. Known {} image size is {}.",
                    large_count,
                    source.verb(),
                    file_word,
                    backend::image_uploader::format_upload_size(threshold),
                    source.verb(),
                    backend::image_uploader::format_upload_size(known_bytes)
                ),
            )
        } else if paths.len() == 1 {
            (
                "Upload image?".to_string(),
                format!(
                    "The {} image will be uploaded to a third-party image host. You may not be able to remove it later.",
                    source.verb()
                ),
            )
        } else {
            (
                "Upload images?".to_string(),
                format!(
                    "The {} {} images will be uploaded to a third-party image host. You may not be able to remove them later.",
                    paths.len(),
                    source.verb()
                ),
            )
        };
        let payload = std::sync::Arc::new(std::sync::Mutex::new(Some(
            backend::ui_actions::PendingConfirmedFileUpload {
                channel: channel.clone(),
                split_id,
                paths,
                source,
                account: backend::ui_actions::composer_account(),
            },
        )));
        self.open_upload_confirmation(
            title,
            format!("After upload, insert the link into #{channel}.\n\n{message}"),
            "Upload".to_string(),
            std::sync::Arc::new(move || {
                if let Some(upload) = payload.lock_or_recover().take() {
                    backend::ui_actions::push_confirmed_file_upload(upload);
                }
            }),
            cx,
        );
    }

    fn open_upload_confirmation(
        &mut self,
        title: String,
        message: String,
        confirm_text: String,
        on_confirm: std::sync::Arc<dyn Fn() + Send + Sync + 'static>,
        cx: &mut Context<Self>,
    ) {
        let modal = self.overlay_manager.read(cx).confirmation_modal.clone();
        modal.update(cx, |modal, cx| {
            modal.open_simple(title, message, confirm_text, false, on_confirm, cx);
        });
        cx.notify();
    }

    /// Toggle emote picker (batched notify - single cx.notify() at end)
    pub fn toggle_emote_picker(&mut self, path: &[usize], window: &Window, cx: &mut Context<Self>) {
        let emote_picker = self.overlay_manager.read(cx).emote_picker.clone();
        let is_open = emote_picker.read(cx).state.is_open;

        if is_open {
            emote_picker.update(cx, |picker, cx| {
                picker.close(true, cx);
            });
        } else {
            let Some((channel, split_id)) = self.split_target_for_path(path, cx) else {
                return;
            };

            let viewport_size = window.viewport_size();
            let x = (viewport_size.width.as_f32()
                - crate::components::overlays::EMOTE_PICKER_MAX_WIDTH
                - 8.0)
                .max(8.0);
            let y = (viewport_size.height.as_f32()
                - crate::components::overlays::EMOTE_PICKER_HEIGHT
                - 8.0)
                .max(8.0);

            emote_picker.update(cx, |picker, _cx| {
                let new_state =
                    crate::components::overlays::EmotePickerState::open(x, y, channel, split_id);
                picker.update_state(new_state);
            });
        }
        cx.notify();
    }

    fn emote_picker_composer_bounds(
        &self,
        split_id: SplitId,
        viewport: Size<Pixels>,
        cx: &App,
    ) -> Option<Bounds<Pixels>> {
        let tab = self.app_state.read(cx).active_tab();
        let path = tab.split_path_for_id(split_id)?;
        let reply_visible = self
            .split_panes
            .get(&split_id)
            .is_some_and(|pane| pane.read(cx).reply_to.is_some());
        let metrics = self
            .chat_input_renderer
            .read(cx)
            .input_surface_metrics_for_split(split_id, reply_visible);
        if self.split_layout.read(cx).is_unified_input_mode() {
            let height = metrics.total_height.min(viewport.height.as_f32());
            return Some(Bounds::new(
                point(px(0.0), viewport.height - px(height)),
                size(viewport.width, px(height)),
            ));
        }
        let leaf = self
            .split_layout
            .read(cx)
            .painted_leaf_bounds(&tab.layout)
            .and_then(|leaves| leaves.into_iter().find(|(candidate, _)| *candidate == path))
            .map(|(_, [left, top, right, bottom])| {
                Bounds::new(
                    point(px(left), px(top)),
                    size(px(right - left), px(bottom - top)),
                )
            })
            .or_else(|| {
                let current = tab.layout.get_channel_at_path(&path).and_then(|stored| {
                    backend::browser_watching::resolve_layout_channel(&stored)
                })?;
                self.file_drop_targets
                    .get(&split_id)
                    .filter(|(painted, _)| current.eq_ignore_ascii_case(painted))
                    .map(|(_, bounds)| *bounds)
            })?;
        let height = metrics.total_height.min(leaf.size.height.as_f32());
        Some(Bounds::new(
            point(leaf.origin.x, leaf.bottom() - px(height)),
            size(leaf.size.width, px(height)),
        ))
    }

    fn dismiss_emote_picker_outside(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) {
        let picker = self.overlay_manager.read(cx).emote_picker.clone();
        if !picker.read(cx).state.is_open {
            return;
        }
        let split_id = picker.read(cx).state.split_id;
        let entity_id = cx.entity_id();
        let Some(viewport) = cx.with_window(entity_id, |window, _| window.viewport_size()) else {
            return;
        };
        if self
            .emote_picker_composer_bounds(split_id, viewport, cx)
            .is_some_and(|bounds| !bounds.contains(&position))
        {
            picker.update(cx, |picker, cx| picker.close(false, cx));
            cx.notify();
        }
    }

    fn process_picker_actions(&mut self, cx: &mut Context<Self>) -> bool {
        let mut processed = false;
        let emote_picker = self.overlay_manager.read(cx).emote_picker.clone();
        if emote_picker.read(cx).has_pending_selection()
            || emote_picker.read(cx).focus_return_requested()
        {
            let (selections, focus_split_id) = emote_picker.update(cx, |picker, _| {
                let mut selections = Vec::new();
                while let Some(selection) = picker.take_pending_selection() {
                    selections.push(selection);
                }
                (selections, picker.take_focus_return_request())
            });
            processed = true;
            for selection in selections {
                if !self.composer_target_is_current(selection.split_id, &selection.channel, cx) {
                    continue;
                }
                self.chat_input_renderer.update(cx, |renderer, cx| {
                    renderer
                        .input_manager
                        .insert_emote(selection.split_id, &selection.name);
                    renderer.reset_composer_affinity(selection.split_id);
                    cx.notify();
                });
                self.notify_split_leaf(selection.split_id, cx);
            }
            if let Some(split_id) = focus_split_id {
                self.focus_split_input(split_id, cx);
            }
        }
        for message in crate::components::overlays::drain_popout_messages() {
            processed = true;
            match message {
                crate::components::overlays::PopoutMessage::EmoteSelected {
                    name,
                    channel,
                    split_id,
                } => {
                    let origin = split_id.and_then(|split_id| {
                        find_open_split_target(self.app_state.read(cx), split_id, "")
                    });
                    let target = if let Some((tab_index, path, stored_channel)) = origin {
                        let Some(current_channel) =
                            backend::browser_watching::resolve_layout_channel(&stored_channel)
                        else {
                            continue;
                        };
                        if !current_channel.eq_ignore_ascii_case(&channel) {
                            continue;
                        }
                        Some((tab_index, path, current_channel))
                    } else {
                        find_open_tab_channel_target(self.app_state.read(cx), &channel)
                    };
                    // No split shows the popout's channel any more: drop the pick rather
                    // than writing it into another channel's draft.
                    let Some((tab_index, target_path, _)) = target else {
                        continue;
                    };
                    self.activate_tab_split(tab_index, target_path.clone(), cx);
                    let Some((_, split_id)) = self.split_target_for_path(&target_path, cx) else {
                        continue;
                    };
                    self.chat_input_renderer.update(cx, |renderer, cx| {
                        renderer.input_manager.insert_emote(split_id, &name);
                        renderer.reset_composer_affinity(split_id);
                        cx.notify();
                    });
                    self.notify_split_leaf(split_id, cx);
                }
                crate::components::overlays::PopoutMessage::Closed => {
                    crate::components::overlays::mark_popout_closed();
                }
            }
        }
        processed
    }

    /// Update autocomplete state based on current input text for a specific split
    ///
    /// Note: `input_text` is passed as parameter to avoid re-entrancy issues when
    /// this is called from within ChatInputRenderer's event handlers.
    pub fn update_autocomplete(
        &mut self,
        _channel: &str,
        path: &[usize],
        input_text: Option<&str>,
        cx: &mut Context<Self>,
    ) {
        use crate::components::overlays::{
            AutocompleteHelper, AutocompleteState, AutocompleteType,
        };

        let Some((resolved_channel, split_id)) = self.split_target_for_path(path, cx) else {
            return;
        };

        let input_text = input_text.map(|s| s.to_string()).unwrap_or_else(|| {
            self.chat_input_renderer
                .read(cx)
                .input_manager
                .input_texts
                .get(&split_id)
                .cloned()
                .unwrap_or_default()
        });
        let input_len = {
            let renderer = self.chat_input_renderer.read(cx);
            let candidate = if renderer.input_manager.input_texts.contains_key(&split_id) {
                renderer
                    .input_manager
                    .cursor_for_split(split_id)
                    .min(input_text.len())
            } else {
                input_text.len()
            };

            if input_text.is_char_boundary(candidate) {
                candidate
            } else {
                input_text
                    .char_indices()
                    .map(|(idx, _)| idx)
                    .take_while(|idx| *idx < candidate)
                    .last()
                    .unwrap_or(0)
            }
        };

        if let Some(context) = AutocompleteHelper::detect_trigger(&input_text, input_len) {
            let min_len = match context.ac_type {
                AutocompleteType::Emote | AutocompleteType::Mention => 0,
                AutocompleteType::Command | AutocompleteType::Tab => 1,
            };

            if context.query.len() >= min_len {
                let new_state =
                    AutocompleteState::open(0.0, 0.0, resolved_channel, split_id, context);
                let autocomplete = self.autocomplete(cx);
                autocomplete.update(cx, |ac, cx| {
                    ac.update_state(new_state);
                    cx.notify();
                });
                return;
            }
        }

        let autocomplete = self.autocomplete(cx);
        if autocomplete.read(cx).state.is_open {
            autocomplete.update(cx, |ac, cx| {
                ac.state.close();
                cx.notify();
            });
        }
    }

    /// Build one uncapped, frozen candidate set for bare-token Tab completion
    /// and apply its first row immediately. The compact `:`/`@` suggestion bar
    /// remains independently available while typing.
    pub(crate) fn begin_inline_tab_completion(
        &mut self,
        channel: &str,
        split_id: SplitId,
        context: crate::components::overlays::CompletionContext,
        reverse: bool,
        cx: &mut Context<Self>,
    ) -> bool {
        use crate::components::overlays::{AutocompleteState, AutocompleteType};

        if context.ac_type != AutocompleteType::Tab {
            return false;
        }

        let Some((stored_target_channel, split_focus_handle)) =
            self.split_panes.get(&split_id).map(|pane| {
                let pane = pane.read(cx);
                (pane.channel.clone(), pane.focus_handle.clone())
            })
        else {
            return false;
        };
        let Some(target_channel) =
            crate::backend::browser_watching::resolve_layout_channel(&stored_target_channel)
        else {
            return false;
        };
        let focus_handle = if backend::settings::get_unified_input_mode() {
            self.input_focus.clone()
        } else {
            split_focus_handle
        };
        if !target_channel.eq_ignore_ascii_case(channel) {
            return false;
        }

        let autocomplete = self.autocomplete(cx);
        autocomplete.update(cx, |autocomplete, _cx| {
            autocomplete.update_state(AutocompleteState::open(
                0.0,
                0.0,
                target_channel,
                split_id,
                context.clone(),
            ));
        });
        let candidates = autocomplete.read(cx).inline_tab_candidates();
        autocomplete.update(cx, |autocomplete, _cx| autocomplete.state.close());

        let applied = self.chat_input_renderer.update(cx, |renderer, cx| {
            let applied = renderer
                .begin_inline_tab_completion(split_id, context, candidates, reverse)
                .is_some();
            if applied {
                cx.notify();
            }
            applied
        });
        if applied {
            self.notify_split_leaf(split_id, cx);
            let entity_id = cx.entity_id();
            let _ = cx.with_window(entity_id, |window, app| {
                window.focus(&focus_handle, app);
            });
            cx.notify();
        }
        applied
    }

    /// Dirty the retained leaf that renders `split_id`.
    ///
    /// The leaf is mounted behind `AnyView::cached`, so GPUI reuses its prepaint
    /// unless that entity itself is dirty. Composer state lives in the shared
    /// `ChatInputRenderer` rather than in the leaf. The active leaf observes the
    /// renderer, so notifying the renderer covers it; a change to a non-active
    /// split's composer, or one that never notifies the renderer, has to name
    /// the leaf here. The alternative, `Window::refresh`, would rebuild every
    /// cached subtree in the window instead of just this one.
    pub(crate) fn notify_split_leaf(&mut self, split_id: SplitId, cx: &mut Context<Self>) {
        if let Some(leaf) = self.split_leaf_views.get(&split_id).cloned() {
            leaf.update(cx, |_, cx| cx.notify());
        }
    }

    /// Commit the currently rendered autocomplete selection through one stale-safe path.
    ///
    /// Keyboard and mouse callers defer here so the chat-input renderer is never updated
    /// re-entrantly from inside one of its own event listeners.
    pub(crate) fn accept_autocomplete(
        &mut self,
        expected_revision: u64,
        index: usize,
        cx: &mut Context<Self>,
    ) -> bool {
        let autocomplete = self.autocomplete(cx);
        let Some(selection) = autocomplete.read(cx).selection_at(expected_revision, index) else {
            return false;
        };

        let Some(stored_target_channel) = self
            .split_panes
            .get(&selection.split_id)
            .map(|pane| pane.read(cx).channel.clone())
        else {
            autocomplete.update(cx, |autocomplete, cx| {
                autocomplete.state.close();
                cx.notify();
            });
            cx.notify();
            return false;
        };
        let Some(target_channel) =
            crate::backend::browser_watching::resolve_layout_channel(&stored_target_channel)
        else {
            autocomplete.update(cx, |autocomplete, cx| {
                autocomplete.state.close();
                cx.notify();
            });
            cx.notify();
            return false;
        };

        if !target_channel.eq_ignore_ascii_case(&selection.channel) {
            autocomplete.update(cx, |autocomplete, cx| {
                autocomplete.state.close();
                cx.notify();
            });
            cx.notify();
            return false;
        }
        let Some(focus_handle) = self.composer_focus_handle(selection.split_id, cx) else {
            autocomplete.update(cx, |autocomplete, cx| {
                autocomplete.state.close();
                cx.notify();
            });
            cx.notify();
            return false;
        };

        let applied = self.chat_input_renderer.update(cx, |renderer, cx| {
            let applied = renderer.input_manager.replace_range_with_token(
                selection.split_id,
                selection.context.replacement_range.clone(),
                &selection.context.expected_fragment,
                &selection.text,
            );
            if applied {
                renderer
                    .input_manager
                    .end_history_navigation(selection.split_id);
                renderer.reset_composer_affinity(selection.split_id);
                cx.notify();
            }
            applied
        });

        if applied
            && selection.is_emote
            && let Some(emote) = autocomplete.read(cx).accepted_emote(
                &selection.channel,
                &selection.text,
                selection.emote_image_url.as_deref(),
            )
        {
            crate::components::overlays::record_emote_usage(&emote);
        }

        autocomplete.update(cx, |autocomplete, cx| {
            autocomplete.state.close();
            cx.notify();
        });

        self.notify_split_leaf(selection.split_id, cx);
        let entity_id = cx.entity_id();
        let _ = cx.with_window(entity_id, |window, app| {
            window.focus(&focus_handle, app);
        });
        cx.notify();
        applied
    }

    /// Close current tab (Ctrl+Shift+W).
    pub fn close_current_tab(&mut self, cx: &mut Context<Self>) {
        let tab_id = self.app_state.read(cx).active_tab().id.clone();
        self.close_tabs_by_id(&tab_id, TabCloseScope::Single, cx);
    }

    fn close_tabs_by_id(
        &mut self,
        tab_id: &str,
        scope: TabCloseScope,
        cx: &mut Context<Self>,
    ) -> bool {
        let closed_channels = self.app_state.update(cx, |state, _| {
            let idx = state.tabs.iter().position(|tab| tab.id == tab_id)?;
            let closing: Vec<_> = state
                .tabs
                .iter()
                .enumerate()
                .filter(|(candidate, _)| match scope {
                    TabCloseScope::Single => *candidate == idx,
                    TabCloseScope::Others => *candidate != idx,
                    TabCloseScope::ToRight => *candidate > idx,
                })
                .map(|(_, tab)| tab)
                .collect();
            let channels = released_channels(state, &closing);
            let generation = state.closed_tab_generation();
            match scope {
                TabCloseScope::Single => {
                    state.remove_tab(idx);
                }
                TabCloseScope::Others => state.close_other_tabs(idx),
                TabCloseScope::ToRight => state.close_tabs_to_right(idx),
            }
            (state.closed_tab_generation() != generation).then_some(channels)
        });
        let Some(mut closed_channels) = closed_channels else {
            return false;
        };
        closed_channels.sort_unstable();
        closed_channels.dedup();

        for channel in closed_channels {
            self.cleanup_channel_caches(&channel, cx);
        }

        self.save_tabs(cx);
        self.refocus_active_input(cx);
        cx.notify();
        true
    }

    /// Bring back the most recently closed tab (or bulk-closed tabs).
    pub fn reopen_closed_tab(&mut self, cx: &mut Context<Self>) -> bool {
        let reopened = self
            .app_state
            .update(cx, |state, cx| state.reopen_closed_tabs(cx));
        self.finish_reopen_closed_tabs(reopened, cx)
    }

    fn finish_reopen_closed_tabs(&mut self, reopened: usize, cx: &mut Context<Self>) -> bool {
        if reopened == 0 {
            return false;
        }
        self.seen_closed_tab_generation = self.app_state.read(cx).closed_tab_generation();
        self.save_tabs(cx);
        self.refocus_active_input(cx);
        cx.notify();
        true
    }

    /// Open a followed live channel picked in the quick switcher: in
    /// `fill_split` while that split is still empty, otherwise where it is
    /// already open, otherwise in a new tab. The opened split takes focus.
    pub(crate) fn open_followed_channel(
        &mut self,
        login: String,
        fill_split: Option<SplitId>,
        cx: &mut Context<Self>,
    ) {
        let empty_target = fill_split.and_then(|split_id| {
            find_open_split_target(self.app_state.read(cx), split_id, "")
                .filter(|(_, _, channel)| is_placeholder(channel))
        });
        if let Some((tab_index, path, _)) = empty_target {
            self.activate_tab_split(tab_index, path.clone(), cx);
            self.set_channel_for_path(path, login, cx);
            return;
        }
        let existing = find_open_tab_channel_target(self.app_state.read(cx), &login);
        if let Some((tab_index, path, _)) = existing {
            self.activate_tab_split(tab_index, path, cx);
            self.refocus_active_input(cx);
            return;
        }
        self.create_new_tab(cx);
        let path = self
            .app_state
            .read(cx)
            .active_tab()
            .valid_active_split_path();
        self.set_channel_for_path(path, login, cx);
    }

    /// Keep visible Live now lists current. A new list version re-reads an
    /// open switcher and repaints the active tab's choosers. `timed_check`
    /// may refetch an expired list, but only while someone is looking: the
    /// window is active and visible and the switcher is open or a chooser's
    /// input has focus.
    fn sync_followed_live(&mut self, timed_check: bool, cx: &mut Context<Self>) -> bool {
        let quick_switcher = self.overlay_manager.read(cx).quick_switcher.clone();
        let switcher_open = quick_switcher.read(cx).is_open;
        let chooser_visible = layout_has_placeholder(&self.app_state.read(cx).active_tab().layout);
        if !switcher_open && !chooser_visible {
            return false;
        }
        if timed_check {
            let chooser_focused = self
                .chat_input_renderer
                .read(cx)
                .input_manager
                .focused_placeholder
                .is_some();
            let window_attended = cx
                .with_window(cx.entity_id(), |window, _| {
                    window.is_window_active() && window.is_visible()
                })
                .unwrap_or(false);
            if window_attended && (switcher_open || chooser_focused) {
                backend::bridge::followed_live::refresh_if_stale();
            }
            if switcher_open && quick_switcher.read(cx).privacy_changed() {
                quick_switcher.update(cx, |switcher, cx| switcher.refresh_followed(cx));
            }
        }
        let version = backend::bridge::followed_live::version();
        if version == self.seen_followed_live_version {
            return false;
        }
        self.seen_followed_live_version = version;
        if switcher_open && quick_switcher.read(cx).followed_version() != version {
            quick_switcher.update(cx, |switcher, cx| switcher.refresh_followed(cx));
        }
        chooser_visible
    }

    /// Open the quick switcher on just the Live now group. A channel chosen
    /// there fills `source` when that split is empty.
    fn open_live_followed_switcher(
        &mut self,
        source: Option<SplitId>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let fill_split = source.filter(|split_id| {
            find_open_split_target(self.app_state.read(cx), *split_id, "")
                .is_some_and(|(_, _, channel)| is_placeholder(&channel))
        });
        let channels = crate::event_handlers::quick_switch_items(self, cx);
        self.overlay_manager.update(cx, |overlays, cx| {
            overlays.open_quick_switcher(
                channels,
                crate::components::overlays::QuickSwitchMode::LiveOnly { fill_split },
                cx,
            );
        });
        let switcher = self.overlay_manager.read(cx).quick_switcher.clone();
        switcher.update(cx, |switcher, cx| switcher.focus_now(window, cx));
        cx.notify();
    }

    pub fn create_new_tab(&mut self, cx: &mut Context<Self>) {
        self.app_state.update(cx, |state, cx| {
            let mut tab = crate::state::TabState::new();
            tab.visibility = state.tab_visibility_mode();
            let idx = state.add_tab(tab, cx);
            state.switch_tab(idx, cx);
        });
        self.save_tabs(cx);
        let active_tab_idx = self.app_state.read(cx).active_tab_idx;
        let active_path = self
            .app_state
            .read(cx)
            .active_tab()
            .valid_active_split_path();
        let split_id = self
            .app_state
            .read(cx)
            .active_tab()
            .layout
            .get_split_id_at_path(&active_path);
        self.pending_new_tab_focus = split_id.map(|split_id| (active_tab_idx, split_id));
        self.refocus_active_input(cx);
        cx.notify();
    }

    /// True, with a toast, when the active tab's split layout is locked. Split
    /// structure changes must check this before any side effect.
    fn refuse_locked_split_layout(&mut self, cx: &mut Context<Self>) -> bool {
        if !self.app_state.read(cx).active_tab().is_locked {
            return false;
        }
        self.overlay_manager.update(cx, |mgr, cx| {
            mgr.toast_manager.update(cx, |toasts, cx| {
                toasts.info("Tab layout is locked".to_string());
                cx.notify();
            });
        });
        cx.notify();
        true
    }

    pub fn create_placeholder_split_adjacent_to_active(
        &mut self,
        direction: AddSplitDirection,
        cx: &mut Context<Self>,
    ) -> Option<Vec<usize>> {
        if self.refuse_locked_split_layout(cx) {
            return None;
        }
        self.cancel_pointer_drags(cx);
        let new_path = self.app_state.update(cx, |state, _cx| {
            state
                .active_tab_mut()
                .insert_placeholder_split_adjacent_to_active(direction)
        });

        if let Some(path) = new_path {
            let active_path = path.clone();
            self.set_active_split(path, cx);
            self.save_workspace_layout(cx);
            cx.notify();
            return Some(active_path);
        }

        None
    }

    fn open_or_focus_whispers_split(&mut self, cx: &mut Context<Self>) -> Option<Vec<usize>> {
        let existing = {
            let app_state = self.app_state.read(cx);
            find_open_tab_channel_target(app_state, backend::whispers::WHISPERS_CHANNEL)
        };
        if let Some((tab_index, path, _channel)) = existing {
            self.activate_tab_split(tab_index, path.clone(), cx);
            return Some(path);
        }

        let path =
            self.create_placeholder_split_adjacent_to_active(AddSplitDirection::Right, cx)?;
        self.set_channel_for_path(
            path.clone(),
            backend::whispers::WHISPERS_CHANNEL.to_string(),
            cx,
        );
        self.set_active_split(path.clone(), cx);
        Some(path)
    }

    pub fn create_placeholder_split_adjacent_to_split(
        &mut self,
        split_id: crate::state::SplitId,
        direction: AddSplitDirection,
        cx: &mut Context<Self>,
    ) -> Option<Vec<usize>> {
        if self.refuse_locked_split_layout(cx) {
            return None;
        }
        let new_path = self.app_state.update(cx, |state, _cx| {
            let tab = state.active_tab_mut();
            let target_path = tab.split_path_for_id(split_id)?;
            tab.insert_placeholder_split_adjacent_to_path(&target_path, direction)
        });

        if let Some(path) = new_path {
            let active_path = path.clone();
            self.set_active_split(path, cx);
            self.save_workspace_layout(cx);
            cx.notify();
            return Some(active_path);
        }

        None
    }

    pub fn select_tab_by_number(&mut self, number: usize, cx: &mut Context<Self>) {
        let changed = self
            .app_state
            .update(cx, |state, cx| state.switch_tab_by_number(number, cx));
        if changed {
            self.save_tabs(cx);
            self.refocus_active_input(cx);
            cx.notify();
        }
    }

    pub fn toggle_live_tabs_only(&mut self, cx: &mut Context<Self>) {
        let previous_tab_id = self.app_state.read(cx).active_tab().id.clone();
        let changed = self
            .app_state
            .update(cx, |state, cx| state.toggle_live_tabs_only(cx));
        if changed {
            self.save_tabs(cx);
            if self.app_state.read(cx).active_tab().id != previous_tab_id {
                self.refocus_active_input(cx);
            }
            cx.notify();
        }
    }

    pub fn toggle_tab_bar_visibility(&mut self, cx: &mut Context<Self>) {
        let changed = self
            .app_state
            .update(cx, |state, _cx| state.toggle_tab_bar_visibility());
        if changed {
            self.save_tabs(cx);
            cx.notify();
        }
    }

    pub fn zoom_in(&mut self, cx: &mut Context<Self>) {
        const FONT_SCALE_STEP: f32 = 0.05;

        let next_scale = backend::settings::get_font_scale() + FONT_SCALE_STEP;
        backend::settings::set_font_scale(next_scale);
        cx.notify();
    }

    pub fn zoom_out(&mut self, cx: &mut Context<Self>) {
        const FONT_SCALE_STEP: f32 = 0.05;

        let next_scale = backend::settings::get_font_scale() - FONT_SCALE_STEP;
        backend::settings::set_font_scale(next_scale);
        cx.notify();
    }

    pub fn reset_zoom(&mut self, cx: &mut Context<Self>) {
        backend::settings::set_font_scale(1.0);
        cx.notify();
    }

    /// PERF: Clean up local caches when a channel is closed
    fn cleanup_channel_caches(&mut self, channel: &str, cx: &mut Context<Self>) {
        let active_channels = self.app_state.read(cx).all_active_channels();

        if !active_channels.contains(channel) {
            self.app_state.update(cx, |state, cx| {
                state.cleanup_orphaned_states(cx);
            });
        }

        let mut valid_ids = self.valid_leaf_split_ids(cx);
        self.file_drop_targets
            .retain(|split_id, _| valid_ids.contains(split_id));
        self.chat_views
            .retain(|split_id, _| valid_ids.contains(split_id));
        valid_ids.extend(self.app_state.read(cx).closed_tab_split_ids());
        self.split_layout
            .update(cx, |layout, _| layout.retain_mod_mode(&valid_ids));

        let renderer = self.chat_input_renderer.clone();
        cx.defer(move |cx| {
            renderer.update(cx, |renderer, _cx| {
                renderer.cleanup_split_ids(&valid_ids);
            });
        });

        if !active_channels.contains(channel) {
            self.overlay_manager.update(cx, |overlays, cx| {
                overlays.cleanup_channel_overlays(channel, cx);
            });
        }
    }

    /// Drop renderer/layout entities that no longer belong to any tab.
    ///
    /// Zed keeps inactive pane entities behind cached GPUI boundaries; Echo
    /// should do the same for tabbed chat splits so switching tabs does not
    /// discard per-split height trees, scroll state, hitboxes, and layout caches.
    fn prune_orphaned_chat_views(&mut self, cx: &mut Context<Self>) -> bool {
        let valid_ids = self.valid_leaf_split_ids(cx);

        if valid_ids.is_empty() {
            return false;
        }

        let before = self.chat_views.len();
        self.chat_views
            .retain(|split_id, _| valid_ids.contains(split_id));
        let removed = before.saturating_sub(self.chat_views.len());
        if removed > 0 {
            crate::log_ui!(
                debug,
                removed,
                retained = self.chat_views.len(),
                "Pruned orphaned chat views"
            );
            return true;
        }

        false
    }

    /// Switch to next tab (Ctrl+Tab)
    pub fn next_tab(&mut self, cx: &mut Context<Self>) {
        let changed = self
            .app_state
            .update(cx, |state, cx| state.switch_tab_relative(1, cx));
        if changed {
            self.save_tabs(cx);
            self.refocus_active_input(cx);
            cx.notify();
        }
    }

    /// Switch to previous tab (Ctrl+Shift+Tab)
    pub fn prev_tab(&mut self, cx: &mut Context<Self>) {
        let changed = self
            .app_state
            .update(cx, |state, cx| state.switch_tab_relative(-1, cx));
        if changed {
            self.save_tabs(cx);
            self.refocus_active_input(cx);
            cx.notify();
        }
    }

    /// Align the render/runtime active split with the selected tab's persisted focus.
    fn sync_active_split_from_active_tab(&mut self, cx: &mut Context<Self>) -> bool {
        let desired_path = self
            .app_state
            .read(cx)
            .active_tab()
            .valid_active_split_path();
        let current_path = self.split_layout.read(cx).get_active_split_path().to_vec();
        if current_path == desired_path {
            return false;
        }

        self.split_layout.update(cx, |layout, _| {
            layout.set_active_split(desired_path.clone())
        });
        self.update_active_chat_view_flags(&desired_path, cx);
        true
    }

    fn process_render_housekeeping(&mut self, cx: &mut Context<Self>) -> bool {
        let mut needs_notify = false;

        let active_tab_changed = self
            .app_state
            .update(cx, |state, cx| state.ensure_active_tab_visible(cx));
        if active_tab_changed {
            self.save_tabs(cx);
            self.refocus_active_input(cx);
            needs_notify = true;
        }

        needs_notify |= self.sync_active_split_from_active_tab(cx);
        needs_notify |= self.restore_abandoned_channel_changes(cx);
        needs_notify |= self.ensure_split_panes_for_layout(cx);
        needs_notify |= self.apply_pending_new_tab_focus(cx);
        let focus_overlay_open = {
            let overlays = self.overlay_manager.read(cx);
            overlays.thread_viewer.read(cx).is_open()
                || overlays.quick_switcher.read(cx).is_open
                || overlays.chatter_list.read(cx).state.is_open
        };
        if std::mem::replace(&mut self.last_focus_overlay_open, focus_overlay_open)
            && !focus_overlay_open
        {
            self.pending_active_input_focus = true;
        }
        needs_notify |= self.apply_pending_active_input_focus(cx);
        needs_notify |= self.apply_initial_focus_if_needed(cx);
        needs_notify |= self.sync_current_window_chrome(cx);
        needs_notify |= self.prewarm_hovered_tab(cx);

        needs_notify
    }

    fn process_ui_invalidations(&mut self, cx: &mut Context<Self>) -> bool {
        let flags = crate::state::ui_invalidation::drain();
        if flags.is_empty() {
            return false;
        }

        let settings_layout =
            flags.contains(crate::state::ui_invalidation::UiInvalidation::SettingsLayout);
        let provider_metadata =
            flags.contains(crate::state::ui_invalidation::UiInvalidation::ProviderMetadata);
        if settings_layout {
            crate::backend::metrics::record_ui_notify_source("app.dirty.settings_layout");
        }
        if provider_metadata {
            crate::backend::metrics::record_ui_notify_source("app.dirty.provider_metadata");
        }

        if settings_layout || provider_metadata {
            let (emote_picker, autocomplete) = {
                let overlays = self.overlay_manager.read(cx);
                (overlays.emote_picker.clone(), overlays.autocomplete.clone())
            };

            if emote_picker.read(cx).state.is_open {
                emote_picker.update(cx, |picker, cx| {
                    if picker.check_for_emote_updates() {
                        cx.notify();
                    }
                });
            }

            autocomplete.update(cx, |autocomplete, cx| {
                if autocomplete.refresh_open_emote_suggestions() {
                    cx.notify();
                }
            });

            crate::components::overlays::refresh_open_emote_picker_popout(cx);
        }
        if flags.contains(crate::state::ui_invalidation::UiInvalidation::WindowState) {
            crate::backend::metrics::record_ui_notify_source("app.dirty.window_state");
        }
        if flags.contains(crate::state::ui_invalidation::UiInvalidation::TopBarState) {
            crate::backend::metrics::record_ui_notify_source("app.dirty.top_bar_state");
        }
        if flags.contains(crate::state::ui_invalidation::UiInvalidation::Shutdown) {
            crate::backend::metrics::record_ui_notify_source("app.dirty.shutdown");
        }

        crate::log_ui!(
            trace,
            dirty_bits = flags.bits(),
            "Drained UI invalidation flags"
        );
        true
    }

    fn focus_split_input(&mut self, split_id: SplitId, cx: &mut Context<Self>) -> bool {
        let placeholder = {
            let tab = self.app_state.read(cx).active_tab();
            tab.split_path_for_id(split_id)
                .and_then(|path| tab.layout.get_channel_at_path(&path))
                .is_some_and(|channel| is_placeholder(&channel))
        };
        if placeholder {
            let Some(input) = self.ensure_channel_chooser(split_id, None, cx) else {
                return false;
            };
            let focus = input.read(cx).focus_handle().clone();
            return cx
                .with_window(cx.entity_id(), |window, app| window.focus(&focus, app))
                .is_some();
        }
        let Some(focus_handle) = self.composer_focus_handle(split_id, cx) else {
            return false;
        };

        let entity_id = cx.entity_id();
        cx.with_window(entity_id, |window, app| {
            window.focus(&focus_handle, app);
        })
        .is_some()
    }

    fn composer_focus_handle(&self, split_id: SplitId, cx: &Context<Self>) -> Option<FocusHandle> {
        let app_state = self.app_state.read(cx);
        let tab = app_state.active_tab();
        let path = tab.split_path_for_id(split_id)?;
        let placeholder = tab
            .layout
            .get_channel_at_path(&path)
            .is_some_and(|channel| is_placeholder(&channel));
        if placeholder {
            Some(
                self.chat_input_renderer
                    .read(cx)
                    .input_manager
                    .placeholder_inputs
                    .get(&split_id)?
                    .input
                    .read(cx)
                    .focus_handle()
                    .clone(),
            )
        } else if backend::settings::get_unified_input_mode() {
            Some(self.input_focus.clone())
        } else {
            self.split_panes
                .get(&split_id)
                .map(|pane| pane.read(cx).focus_handle.clone())
        }
    }

    fn apply_initial_focus_if_needed(&mut self, cx: &mut Context<Self>) -> bool {
        if self.initial_focus_applied {
            return false;
        }

        let Some((_channel, _path, split_id)) = self.active_split_target(cx) else {
            return false;
        };
        if self.focus_split_input(split_id, cx) {
            self.initial_focus_applied = true;
            return true;
        }

        false
    }

    fn apply_pending_new_tab_focus(&mut self, cx: &mut Context<Self>) -> bool {
        let Some((tab_idx, split_id)) = self.pending_new_tab_focus else {
            return false;
        };
        let active_path = self
            .app_state
            .read(cx)
            .active_tab()
            .valid_active_split_path();
        let active_tab_idx = self.app_state.read(cx).active_tab_idx;
        let active_split_id = self
            .app_state
            .read(cx)
            .active_tab()
            .layout
            .get_split_id_at_path(&active_path);
        if active_tab_idx != tab_idx || active_split_id != Some(split_id) {
            self.pending_new_tab_focus = None;
            return false;
        }
        if !self.focus_split_input(split_id, cx) {
            return false;
        }
        if self
            .chat_input_renderer
            .read(cx)
            .input_manager
            .placeholder_inputs
            .contains_key(&split_id)
        {
            self.chat_input_renderer.update(cx, |renderer, _| {
                renderer.input_manager.focused_placeholder = Some(split_id);
            });
        }
        self.pending_new_tab_focus = None;
        true
    }

    /// Move keyboard focus to the active split's input after the active split
    /// changed. The focus itself is deferred until the current event dispatch
    /// releases the window.
    pub(crate) fn refocus_active_input(&mut self, cx: &mut Context<Self>) {
        self.sync_active_split_from_active_tab(cx);
        self.pending_active_input_focus = true;
        let this = cx.weak_entity();
        cx.defer(move |cx| {
            let _ = this.update(cx, |app, cx| {
                if app.apply_pending_active_input_focus(cx) {
                    cx.notify();
                }
            });
        });
    }

    fn active_input_focus_target(
        &self,
        cx: &Context<Self>,
    ) -> Option<(SplitId, FocusHandle, bool)> {
        let app_state = self.app_state.read(cx);
        let tab = app_state.active_tab();
        let path = tab.valid_active_split_path();
        let split_id = tab.layout.get_split_id_at_path(&path)?;
        let placeholder = tab
            .layout
            .get_channel_at_path(&path)
            .is_some_and(|channel| is_placeholder(&channel));
        let handle = self.composer_focus_handle(split_id, cx)?;
        Some((split_id, handle, placeholder))
    }

    fn apply_pending_active_input_focus(&mut self, cx: &mut Context<Self>) -> bool {
        if !self.pending_active_input_focus {
            return false;
        }
        let text_overlay_open = self.overlay_manager.read(cx).text_overlay_open(cx);
        self.ensure_split_panes_for_layout(cx);
        let placeholder_split = {
            let tab = self.app_state.read(cx).active_tab();
            let path = tab.valid_active_split_path();
            tab.layout
                .get_channel_at_path(&path)
                .filter(|channel| is_placeholder(channel))
                .and_then(|_| tab.layout.get_split_id_at_path(&path))
        };
        if let Some(split_id) = placeholder_split {
            self.ensure_channel_chooser(split_id, None, cx);
        }
        let Some((split_id, focus_handle, placeholder)) = self.active_input_focus_target(cx) else {
            self.pending_active_input_focus = false;
            return false;
        };
        let entity_id = cx.entity_id();
        let this = &*self;
        let Some(focused) = cx.with_window(entity_id, |window, app| {
            if this
                .tab_bar_renderer
                .read(app)
                .is_rename_input_focused(window, app)
                || (text_overlay_open && !this.focus_accepts_typed_redirect(window, app))
            {
                return false;
            }
            if !focus_handle.is_focused(window) {
                window.focus(&focus_handle, app);
            }
            true
        }) else {
            return false;
        };
        self.pending_active_input_focus = false;
        if focused && placeholder {
            self.chat_input_renderer.update(cx, |renderer, _| {
                renderer.input_manager.focused_placeholder = Some(split_id);
            });
        }
        focused
    }

    /// Whether a chat composer (any split's, or the unified one) has focus.
    pub(crate) fn composer_owns_focus(&self, window: &Window, cx: &App) -> bool {
        self.input_focus.is_focused(window)
            || self
                .split_panes
                .values()
                .any(|pane| pane.read(cx).focus_handle.is_focused(window))
    }

    /// Whether keyboard focus sits somewhere typed text may be taken from:
    /// nothing, the app root, a chat canvas, an inactive split input, or a
    /// handle whose element is no longer rendered.
    fn focus_accepts_typed_redirect(&self, window: &Window, cx: &App) -> bool {
        let Some(focused) = window.focused(cx) else {
            return true;
        };
        if focused == self.focus_handle
            || focused == self.input_focus
            || !self.focus_handle.contains_focused(window, cx)
        {
            return true;
        }
        self.split_panes
            .values()
            .any(|pane| pane.read(cx).focus_handle == focused)
            || self
                .chat_views
                .values()
                .any(|view| view.read(cx).canvas_list.read(cx).focus_handle == focused)
    }

    /// Type-anywhere: a printable key pressed while no text field owns focus
    /// focuses the active split's input and is typed there.
    /// Focus the active split's input and type `text` there. `None` only
    /// focuses it, so a pending dead key can compose in the input.
    pub(crate) fn redirect_typed_text_to_active_input(
        &mut self,
        text: Option<&str>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if crate::components::ui::input::is_input_focused(window) {
            return false;
        }
        // A focused tab stop outside any text overlay is a control (a button,
        // a slider), never a text field, so printable keys still go to chat.
        let focused_control = window.focused(cx).is_some_and(|focused| focused.tab_stop);
        if self.overlay_manager.read(cx).text_overlay_open(cx)
            || !(focused_control || self.focus_accepts_typed_redirect(window, cx))
        {
            return false;
        }
        self.sync_active_split_from_active_tab(cx);
        self.ensure_split_panes_for_layout(cx);
        let Some((split_id, focus_handle, placeholder)) = self.active_input_focus_target(cx) else {
            return false;
        };
        if focus_handle.is_focused(window) {
            return false;
        }
        if placeholder {
            let Some(input) = self.ensure_channel_chooser(split_id, Some(window), cx) else {
                return false;
            };
            input.read(cx).focus_handle().clone().focus(window, cx);
            if let Some(text) = text {
                input.update(cx, |input, cx| input.insert(text, window, cx));
            }
            self.pending_active_input_focus = false;
            cx.notify();
            return true;
        }
        let Some((channel, path, target_split_id)) = self.active_split_target(cx) else {
            return false;
        };
        if target_split_id != split_id {
            return false;
        }
        focus_handle.focus(window, cx);
        self.pending_active_input_focus = false;
        let draft = self.chat_input_renderer.update(cx, |renderer, cx| {
            text.and_then(|text| renderer.insert_typed_text(split_id, text, false, cx))
        });
        if text.is_some() {
            self.update_autocomplete(&channel, &path, draft.as_deref(), cx);
        }
        self.notify_split_leaf(split_id, cx);
        cx.notify();
        true
    }

    fn sync_current_window_chrome(&mut self, cx: &mut Context<Self>) -> bool {
        let before_title = self.last_window_title.clone();
        let before_transparent = self.last_window_transparent;
        let before_top = self.last_window_always_on_top;
        let entity_id = cx.entity_id();

        let updated = cx.with_window(entity_id, |window, _app| {
            self.sync_main_window_appearance(window);
            self.update_window_title(window);
        });

        updated.is_some()
            && (before_title != self.last_window_title
                || before_transparent != self.last_window_transparent
                || before_top != self.last_window_always_on_top)
    }

    pub fn popup_active_split(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(channel) = self.active_split_channel(cx) else {
            return false;
        };

        let app_state = self.app_state.clone();
        crate::components::chat::open_channel_popup(channel, app_state, cx);
        true
    }

    pub fn popup_active_tab(&mut self, cx: &mut Context<Self>) -> bool {
        self.popup_active_tab_channels(cx)
    }

    pub fn popup_active_tab_channels(&mut self, cx: &mut Context<Self>) -> bool {
        let channels = self.app_state.read(cx).active_tab().channels();
        if channels.is_empty() {
            return false;
        }

        let app_state = self.app_state.clone();
        for channel in channels {
            crate::components::chat::open_channel_popup(channel, app_state.clone(), cx);
        }
        true
    }

    pub fn reconnect_active_channel(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(channel) = self.active_split_channel(cx) else {
            return false;
        };

        backend::irc::reconnect_with_current_auth();
        backend::connect_channel(&channel);
        let last_hashes = self
            .app_state
            .read(cx)
            .stream_info
            .get(&channel)
            .map(|info| {
                (
                    channel.clone(),
                    backend::stream_info_gate_hash(info.as_ref()),
                )
            })
            .into_iter()
            .collect();
        if !backend::queue_stream_info_refresh(vec![channel.clone()], last_hashes) {
            crate::log_net!(
                debug,
                "Reconnect stream refresh skipped for {channel}: poll in flight"
            );
        }
        cx.notify();
        true
    }

    pub fn start_change_active_split_channel(&mut self, cx: &mut Context<Self>) -> bool {
        let active_path = self.split_layout.read(cx).get_active_split_path().to_vec();
        let current_channel = self
            .app_state
            .read(cx)
            .active_tab()
            .layout
            .get_channel_at_path(&active_path)
            .filter(|channel| !is_placeholder(channel));
        let Some(current_channel) = current_channel else {
            return false;
        };
        let chooser_channel =
            crate::backend::browser_watching::resolve_layout_channel(&current_channel)
                .unwrap_or_default();

        let placeholder = crate::state::new_placeholder();
        let changed = self.app_state.update(cx, |state, cx| {
            let split_id = state.active_tab().layout.get_split_id_at_path(&active_path);
            let tab = state.active_tab_mut();
            if tab.set_channel_at_path(&active_path, placeholder) {
                if let Some(split_id) = split_id {
                    state
                        .pending_channel_changes
                        .insert(split_id, current_channel.clone());
                }
                cx.notify();
                true
            } else {
                false
            }
        });

        if !changed {
            return false;
        }

        if let Some(split_id) = self
            .app_state
            .read(cx)
            .active_tab()
            .layout
            .get_split_id_at_path(&active_path)
        {
            let this = cx.weak_entity();
            cx.defer(move |cx| {
                let _ = this.update(cx, |this, cx| {
                    if let Some(input) = this.ensure_channel_chooser(split_id, None, cx) {
                        cx.with_window(cx.entity_id(), |window, app| {
                            input.update(app, |state, cx| {
                                state.set_value(chooser_channel, window, cx)
                            });
                        });
                        this.focus_split_input(split_id, cx);
                        cx.notify();
                    }
                });
            });
            self.notify_split_leaf(split_id, cx);
        }
        cx.notify();
        true
    }

    /// Dismiss an open channel chooser: the split shows its previous channel
    /// again, with the same chat view and scroll position.
    pub fn restore_pending_channel_change(
        &mut self,
        split_id: crate::state::SplitId,
        cx: &mut Context<Self>,
    ) -> bool {
        let (restored, orphaned) = self.app_state.update(cx, |state, _| {
            let previous = state.pending_channel_changes.get(&split_id).cloned();
            let restored = state.restore_pending_channel_change(split_id);
            (restored, previous.filter(|_| !restored))
        });
        if let Some(previous) = orphaned {
            let previous = crate::backend::browser_watching::resolve_layout_channel(&previous)
                .unwrap_or(previous);
            self.cleanup_channel_caches(&previous, cx);
        }
        if !restored {
            return false;
        }
        self.chat_input_renderer.update(cx, |renderer, _cx| {
            renderer.input_manager.placeholder_inputs.remove(&split_id);
            if renderer.input_manager.focused_placeholder == Some(split_id) {
                renderer.input_manager.focused_placeholder = None;
            }
        });
        let is_active = self
            .app_state
            .read(cx)
            .active_tab()
            .layout
            .get_split_id_at_path(self.split_layout.read(cx).get_active_split_path())
            == Some(split_id);
        if is_active {
            self.refocus_active_input(cx);
        }
        self.notify_split_leaf(split_id, cx);
        cx.notify();
        true
    }

    /// A chooser left behind by focusing another split or tab is dismissed.
    fn restore_abandoned_channel_changes(&mut self, cx: &mut Context<Self>) -> bool {
        if self.app_state.read(cx).pending_channel_changes.is_empty() {
            return false;
        }
        let active_split_id = self
            .app_state
            .read(cx)
            .active_tab()
            .layout
            .get_split_id_at_path(self.split_layout.read(cx).get_active_split_path());
        let abandoned: Vec<_> = self
            .app_state
            .read(cx)
            .pending_channel_changes
            .keys()
            .copied()
            .filter(|split_id| Some(*split_id) != active_split_id)
            .collect();
        let mut restored = false;
        for split_id in abandoned {
            restored |= self.restore_pending_channel_change(split_id, cx);
        }
        restored
    }

    pub fn focus_adjacent_split(
        &mut self,
        direction: AddSplitDirection,
        cx: &mut Context<Self>,
    ) -> bool {
        let target_path = {
            let tab = self.app_state.read(cx).active_tab();
            match self.split_layout.read(cx).painted_leaf_bounds(&tab.layout) {
                Some(bounds) => tab.adjacent_split_path_in_bounds(direction, bounds),
                None => tab.adjacent_split_path(direction),
            }
        };
        let Some(path) = target_path else {
            return false;
        };

        self.set_active_split(path, cx);
        true
    }

    fn active_split_channel(&self, cx: &Context<Self>) -> Option<String> {
        let path = self.split_layout.read(cx).get_active_split_path().to_vec();
        let stored_channel = self
            .app_state
            .read(cx)
            .active_tab()
            .layout
            .get_channel_at_path(&path)
            .filter(|channel| !is_placeholder(channel))?;
        crate::backend::browser_watching::resolve_layout_channel(&stored_channel)
    }

    fn update_active_chat_view_flags(&self, path: &[usize], cx: &mut Context<Self>) {
        let active_split_id = self
            .app_state
            .read(cx)
            .active_tab()
            .layout
            .get_split_id_at_path(path);

        for (split_id, view) in &self.chat_views {
            let is_active = active_split_id == Some(*split_id);
            view.update(cx, |view, cx| {
                view.canvas_list.update(cx, |list, _cx| {
                    list.set_active(is_active);
                });
            });
        }
    }

    /// Propagate modifier hold state directly into every retained chat canvas.
    /// GPUI memoizes child views, so notifying only the app root would leave a
    /// stationary chat input unaware of an Alt press or release.
    pub(crate) fn sync_chat_pause_modifiers(
        &mut self,
        control: bool,
        alt: bool,
        shift: bool,
        platform: bool,
        window_active: bool,
        cx: &mut Context<Self>,
    ) -> bool {
        let (control, alt, shift, platform) = if window_active {
            (control, alt, shift, platform)
        } else {
            (false, false, false, false)
        };
        let policy = backend::settings::get_chat_pause_modifier();
        let state = (control, alt, shift, platform, policy);
        if self.last_chat_pause_modifier_state == Some(state) {
            return false;
        }
        self.last_chat_pause_modifier_state = Some(state);

        let mut changed = false;
        for chat_view in self.chat_views.values() {
            let canvas_list = chat_view.read(cx).canvas_list.clone();
            if !canvas_list.read(cx).is_tab_visible() {
                continue;
            }
            changed |= canvas_list.update(cx, |list, cx| {
                let changed = list.set_chat_pause_modifiers(control, alt, shift, platform);
                if changed {
                    crate::backend::metrics::record_ui_notify_source("app.chat_pause.modifier");
                    cx.notify();
                }
                changed
            });
        }
        changed
    }

    /// Stop retained inactive-tab canvas animation loops without discarding their
    /// message, layout, scroll, or prepared-paint caches.
    fn sync_visible_tab_chat_views(&mut self, cx: &mut Context<Self>) {
        {
            let app_state = self.app_state.read(cx);
            if self.last_rendered_tab_id.as_deref() == Some(app_state.active_tab().id.as_str()) {
                return;
            }
        }

        self.cancel_stale_pointer_drags(cx);

        let (tab_id, visible_split_ids) = {
            let app_state = self.app_state.read(cx);
            let active_tab = app_state.active_tab();
            (
                active_tab.id.clone(),
                active_tab
                    .layout
                    .collect_split_ids()
                    .into_iter()
                    .collect::<std::collections::HashSet<_>>(),
            )
        };

        self.last_rendered_tab_id = Some(tab_id);
        let pause_modifiers = self
            .last_chat_pause_modifier_state
            .map(|(control, alt, shift, platform, _policy)| (control, alt, shift, platform));

        for (split_id, chat_view) in &self.chat_views {
            let visible = visible_split_ids.contains(split_id);
            chat_view.update(cx, |view, cx| {
                let changed = view.canvas_list.update(cx, |list, cx| {
                    let visibility_changed = list.set_tab_visible(visible);
                    let modifier_changed = if visible {
                        pause_modifiers.is_some_and(|(control, alt, shift, platform)| {
                            list.set_chat_pause_modifiers(control, alt, shift, platform)
                        })
                    } else {
                        false
                    };
                    let changed = visibility_changed || modifier_changed;
                    if changed && visible {
                        cx.notify();
                    }
                    changed
                });

                if changed && visible {
                    view.mark_dirty_for(crate::components::chat::ChatViewDirtyReason::RenderParams);
                    cx.notify();
                }
            });
        }
    }

    /// Focus the chat input
    pub fn focus_input(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let placeholder_split = {
            let tab = self.app_state.read(cx).active_tab();
            let path = tab.valid_active_split_path();
            tab.layout
                .get_channel_at_path(&path)
                .filter(|channel| is_placeholder(channel))
                .and_then(|_| tab.layout.get_split_id_at_path(&path))
        };
        if let Some(split_id) = placeholder_split
            && let Some(input) = self.ensure_channel_chooser(split_id, Some(window), cx)
        {
            input.read(cx).focus_handle().clone().focus(window, cx);
            return;
        }
        if backend::settings::get_unified_input_mode() {
            self.input_focus.focus(window, cx);
            return;
        }

        let active_path = self.split_layout.read(cx).get_active_split_path().to_vec();
        let split_id = self
            .app_state
            .read(cx)
            .active_tab()
            .layout
            .get_split_id_at_path(&active_path);
        if let Some(focus_handle) = split_id
            .and_then(|split_id| self.split_panes.get(&split_id))
            .map(|pane| pane.read(cx).focus_handle.clone())
        {
            focus_handle.focus(window, cx);
        } else {
            self.input_focus.focus(window, cx);
        }
    }

    /// Get the current cursor style based on global drag state
    /// Returns Some(cursor) if drag is active, None for default behavior
    fn current_cursor(&self, cx: &Context<Self>) -> Option<CursorStyle> {
        let layout = self.split_layout.read(cx);

        if let Some(drag) = layout.resize_drag() {
            return Some(match drag.orientation {
                SplitOrientation::Vertical => CursorStyle::ResizeLeftRight,
                SplitOrientation::Horizontal => CursorStyle::ResizeUpDown,
            });
        }

        if layout.is_dragging_pane() {
            return Some(CursorStyle::ClosedHand);
        }

        None
    }

    pub(crate) fn cancel_pointer_drags(&mut self, cx: &mut Context<Self>) -> bool {
        let split_dragging = {
            let layout = self.split_layout.read(cx);
            layout.is_resizing() || layout.is_dragging_pane() || layout.pending_pane_drag.is_some()
        };
        let tab_dragging = {
            let renderer = self.tab_bar_renderer.read(cx);
            renderer.is_tab_drag_pending() || renderer.is_tab_dragging()
        };
        if !split_dragging && !tab_dragging {
            return false;
        }
        self.end_split_drag(cx);
        self.split_layout
            .update(cx, |layout, _| layout.cancel_pane_drag());
        self.tab_bar_renderer
            .update(cx, |renderer, _| renderer.cancel_tab_drag());
        cx.notify();
        true
    }

    pub(crate) fn cancel_stale_pointer_drags(&mut self, cx: &mut Context<Self>) {
        let tab_id = &self.app_state.read(cx).active_tab().id;
        let stale_split = {
            let layout = self.split_layout.read(cx);
            layout
                .resize_drag
                .as_ref()
                .is_some_and(|drag| drag.tab_id != *tab_id)
                || layout
                    .pane_drag
                    .as_ref()
                    .is_some_and(|drag| drag.tab_id != *tab_id)
                || layout
                    .pending_pane_drag
                    .as_ref()
                    .is_some_and(|drag| drag.tab_id != *tab_id)
        };
        let stale_tab = {
            let renderer = self.tab_bar_renderer.read(cx);
            (renderer.is_tab_drag_pending() || renderer.is_tab_dragging())
                && renderer.tab_manager.drag_state.source_tab_id != *tab_id
        };
        if stale_split {
            self.end_split_drag(cx);
            self.split_layout
                .update(cx, |layout, _| layout.cancel_pane_drag());
        }
        if stale_tab {
            self.tab_bar_renderer
                .update(cx, |renderer, _| renderer.cancel_tab_drag());
        }
        if stale_split || stale_tab {
            cx.notify();
        }
    }

    /// Capture tracking survives occluding overlays and releases outside the root.
    /// While a split is being moved, a transparent layer over the window takes
    /// the pointer so chat rows, usernames, links and tooltips underneath do not
    /// react to a pointer that is only passing through. Because it also hides
    /// the root's mouse handlers, it tracks the drop target and finishes the
    /// drop itself, and it paints the full-span edge drop preview.
    fn render_pane_drag_shield(
        &self,
        cursor: Option<CursorStyle>,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let layout = self.split_layout.read(cx);
        if !layout.is_dragging_pane() {
            return None;
        }
        let split_area = layout.split_area_bounds();
        let edge_preview = layout.root_edge_preview();
        let accent = colors::accent();
        Some(
            div()
                .id("pane-drag-shield")
                .absolute()
                .inset_0()
                .occlude()
                .cursor(cursor.unwrap_or(CursorStyle::ClosedHand))
                .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, window, cx| {
                    this.update_pane_drag_from_pointer(
                        event.position,
                        window.modifiers().control,
                        cx,
                    );
                }))
                .on_mouse_up(
                    MouseButton::Left,
                    cx.listener(|this, event: &MouseUpEvent, _window, cx| {
                        this.finish_pointer_drags(event.position, cx);
                    }),
                )
                .child(
                    canvas(
                        |_bounds, _window, _cx| {},
                        move |_bounds, _: (), window, _cx| {
                            // Thin edge hints say the outer edges are drop targets too.
                            if let Some(area) = split_area {
                                let hint = px(3.0);
                                let hints = [
                                    Bounds::new(area.origin, size(area.size.width, hint)),
                                    Bounds::new(
                                        point(area.origin.x, area.bottom() - hint),
                                        size(area.size.width, hint),
                                    ),
                                    Bounds::new(area.origin, size(hint, area.size.height)),
                                    Bounds::new(
                                        point(area.right() - hint, area.origin.y),
                                        size(hint, area.size.height),
                                    ),
                                ];
                                for bounds in hints {
                                    window.paint_quad(fill(bounds, accent.opacity(0.35)));
                                }
                            }
                            if let Some(preview) = edge_preview {
                                window.paint_quad(fill(preview, accent.opacity(0.18)));
                                window.paint_quad(outline(
                                    preview,
                                    accent.opacity(0.85),
                                    BorderStyle::Solid,
                                ));
                            }
                        },
                    )
                    .absolute()
                    .inset_0(),
                )
                .into_any_element(),
        )
    }

    fn update_pane_drag_from_pointer(
        &mut self,
        position: Point<Pixels>,
        ctrl: bool,
        cx: &mut Context<Self>,
    ) {
        let changed = self.split_layout.update(cx, |layout, _| {
            if !ctrl && layout.pane_drag_needs_ctrl() {
                layout.cancel_pane_drag();
                return true;
            }
            if !layout.is_dragging_pane() {
                return false;
            }
            layout.update_pane_drop_target(position);
            true
        });
        if changed {
            crate::managers::frame_rate_controller::record_interaction();
            cx.notify();
        }
    }

    /// Finish whichever pointer drag a left-button release ends.
    pub(crate) fn finish_pointer_drags(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) {
        crate::managers::frame_rate_controller::record_interaction();
        self.cancel_stale_pointer_drags(cx);
        self.split_layout.update(cx, |layout, _| {
            layout.clear_pending_pane_drag();
            if layout.is_dragging_pane() {
                layout.update_pane_drop_target(position);
            }
        });
        self.end_split_drag(cx);
        self.end_pane_drag(cx);
        self.end_tab_drag(cx);
    }

    fn render_drag_tracker(&self, cx: &Context<Self>) -> Option<AnyElement> {
        let layout = self.split_layout.read(cx);
        let renderer = self.tab_bar_renderer.read(cx);
        if !layout.is_resizing()
            && !layout.is_dragging_pane()
            && layout.pending_pane_drag.is_none()
            && !renderer.is_tab_drag_pending()
            && !renderer.is_tab_dragging()
        {
            return None;
        }
        let app = cx.entity().downgrade();
        Some(
            canvas(
                |_bounds, _window, _cx| {},
                move |_bounds, _: (), window, _cx| {
                    let move_app = app.clone();
                    window.on_mouse_event(move |event: &MouseMoveEvent, phase, _window, cx| {
                        if phase != DispatchPhase::Capture {
                            return;
                        }
                        let _ = move_app.update(cx, |this, cx| {
                            this.cancel_stale_pointer_drags(cx);
                            if event.pressed_button != Some(MouseButton::Left) {
                                this.cancel_pointer_drags(cx);
                                return;
                            }
                            crate::managers::frame_rate_controller::record_interaction();
                            this.update_split_drag(event.position, cx);
                        });
                    });
                    window.on_mouse_event(move |event: &MouseUpEvent, phase, _window, cx| {
                        if phase != DispatchPhase::Capture || event.button != MouseButton::Left {
                            return;
                        }
                        let _ = app.update(cx, |this, cx| this.end_split_drag_from_pointer(cx));
                        // Let an in-window drop finish in bubble phase; swallowed or
                        // off-window releases cancel instead of applying a stale target.
                        let app = app.clone();
                        cx.defer(move |cx| {
                            let _ = app.update(cx, |this, cx| this.cancel_pointer_drags(cx));
                        });
                    });
                },
            )
            .absolute()
            .inset_0()
            .into_any_element(),
        )
    }
}

impl Drop for EchoApp {
    fn drop(&mut self) {
        let shutdown_start = Instant::now();
        crate::log_ui!(info, "EchoApp dropped - requesting backend shutdown");
        config::flush_tab_saves(std::time::Duration::from_secs(2));
        backend::shutdown();
        crate::log_ui!(
            info,
            elapsed_ms = shutdown_start.elapsed().as_millis(),
            "EchoApp drop shutdown path complete"
        );
    }
}

/// Channels that closing these tabs releases, including the channel a split
/// with an open chooser still shows underneath.
fn released_channels(
    state: &crate::state::AppState,
    closing: &[&crate::state::TabState],
) -> Vec<String> {
    let mut channels: Vec<_> = closing.iter().flat_map(|tab| tab.channels()).collect();
    channels.extend(
        state
            .pending_channel_changes
            .iter()
            .filter(|(split_id, _)| {
                closing
                    .iter()
                    .any(|tab| tab.split_path_for_id(**split_id).is_some())
            })
            .map(|(_, previous)| {
                crate::backend::browser_watching::resolve_layout_channel(previous)
                    .unwrap_or_else(|| previous.clone())
            }),
    );
    channels
}

impl Render for EchoApp {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        crate::utils::drain_render_image_drops_coalesced(window, cx);
        crate::theme::colors::begin_main_window_frame();
        crate::utils::remember_main_window_display(window, cx);
        crate::backend::metrics::record_app_render();
        let modifiers = window.modifiers();
        let _ = self.sync_chat_pause_modifiers(
            modifiers.control,
            modifiers.alt,
            modifiers.shift,
            modifiers.platform,
            window.is_window_active(),
            cx,
        );
        self.sync_visible_tab_chat_views(cx);
        if *crate::log::RENDER_TRACE_VERBOSE {
            crate::log_perf_render!(debug, chat_views = self.chat_views.len(), "app.render");
        }
        let _frame_guard = crate::fps_profiler::FrameTimingGuard::begin();

        let setup_start = crate::fps_profiler::phase_timer();

        let viewport_size = window.viewport_size();
        let window_width: f32 = viewport_size.width.into();
        let window_height: f32 = viewport_size.height.into();

        let (layout, highlights_enabled) = {
            let active_tab = self.app_state.read(cx).active_tab();
            (active_tab.layout.clone(), active_tab.highlights_enabled)
        };

        let global_cursor = self.current_cursor(cx);
        crate::fps_profiler::record_phase_since("setup", setup_start);

        let tabbar_start = crate::fps_profiler::phase_timer();
        let (tab_bar_stale, tab_bar_height, requires_dedicated_tab_row, inline_tab_demand) =
            self.tab_bar_renderer.update(cx, |renderer, cx| {
                let stale = renderer.prepare(window, cx);
                (
                    stale,
                    renderer.effective_tab_bar_height(),
                    renderer.requires_dedicated_tab_row(),
                    renderer.inline_tab_demand(),
                )
            });
        crate::fps_profiler::record_phase_since("tabbar", tabbar_start);

        let inline_tabs = backend::settings::get_tab_placement()
            == backend::settings::TabPlacement::Inline
            && !requires_dedicated_tab_row;
        let titlebar_height = backend::settings::titlebar_height_px();
        let chrome_height = titlebar_height + if inline_tabs { 0.0 } else { tab_bar_height };

        let topbar_start = crate::fps_profiler::phase_timer();
        let top_bar_stale = self.top_bar_view.update(cx, |view, cx| {
            view.prepare(
                inline_tabs,
                inline_tab_demand,
                tab_bar_height,
                tab_bar_stale,
                window,
                cx,
            )
        });
        let top_bar = crate::components::top_bar::TopBarView::view_element(
            &self.top_bar_view,
            titlebar_height,
            top_bar_stale,
        );
        let dedicated_tab_bar = (!inline_tabs).then(|| {
            TabBarRenderer::view_element(&self.tab_bar_renderer, tab_bar_height, tab_bar_stale)
        });
        crate::fps_profiler::record_phase_since("topbar", topbar_start);

        let content_start = crate::fps_profiler::phase_timer();
        let main_content = self.render_main_content_section(
            &layout,
            highlights_enabled,
            window_width,
            window_height,
            chrome_height,
            window,
            cx,
        );
        crate::fps_profiler::record_phase_since("content", content_start);
        crate::startup_metrics::record("first_root_render");
        self.record_first_chat_view_usable(cx);

        let input_start = crate::fps_profiler::phase_timer();
        let input = self.render_unified_input(window, cx);
        crate::fps_profiler::record_phase_since("input", input_start);

        let overlay_start = crate::fps_profiler::phase_timer();
        let chatterino_welcome = self.render_chatterino_welcome(chrome_height, cx);
        let overlays = self.render_all_overlays(window, chrome_height, cx);
        crate::fps_profiler::record_phase_since("overlays", overlay_start);
        let drag_tracker = self.render_drag_tracker(cx);
        let drag_shield = self.render_pane_drag_shield(global_cursor, cx);

        let assemble_start = crate::fps_profiler::phase_timer();

        let result = self
            .register_event_handlers(
                div()
                    .id("echo-app")
                    .key_context("EchoApp")
                    .track_focus(&self.focus_handle)
                    .size_full()
                    .min_w_0()
                    .min_h_0()
                    .flex()
                    .flex_col()
                    .bg(colors::bg_void())
                    .text_color(colors::text_primary())
                    .font_family(echo_chat_render::text_run_intern::ui_font_family())
                    .when_some(global_cursor, |div, cursor| div.cursor(cursor)),
                window,
                cx,
            )
            .child(top_bar)
            .children(dedicated_tab_bar)
            .child(main_content)
            .children(input)
            .children(chatterino_welcome)
            .children(overlays)
            .children(drag_shield)
            .children(drag_tracker)
            .when(crate::fps_profiler::is_overlay_enabled(), |div| {
                div.child(crate::fps_profiler::render_fps_overlay())
            });
        crate::fps_profiler::record_phase_since("assemble", assemble_start);

        result
    }
}

impl EchoApp {
    pub(crate) fn sync_main_window_appearance(&mut self, window: &mut Window) {
        let window_transparent = crate::backend::settings::get_window_transparent_cached();
        if self.last_window_transparent != Some(window_transparent) {
            window.set_background_appearance(if window_transparent {
                WindowBackgroundAppearance::Transparent
            } else {
                WindowBackgroundAppearance::Opaque
            });
            self.last_window_transparent = Some(window_transparent);
            window.refresh();
        }

        let always_on_top = crate::backend::settings::get_always_on_top();
        if self.last_window_always_on_top != Some(always_on_top) {
            #[cfg(any(target_os = "windows", target_os = "linux"))]
            {
                if apply_main_window_always_on_top(window, always_on_top) {
                    self.last_window_always_on_top = Some(always_on_top);
                }
            }
            #[cfg(not(any(target_os = "windows", target_os = "linux")))]
            {
                self.last_window_always_on_top = Some(always_on_top);
            }
        }
    }

    /// Poll and update embed metadata version, invalidating layouts if changed.
    fn update_embed_metadata(&mut self, cx: &mut Context<Self>) -> bool {
        use std::sync::atomic::{AtomicU32, Ordering};
        static LAST_EMBED_VERSION: AtomicU32 = AtomicU32::new(0);

        let current_version = backend::bridge::get_embed_metadata_version();
        let last_version = LAST_EMBED_VERSION.load(Ordering::Relaxed);

        if current_version == last_version {
            return false;
        }

        LAST_EMBED_VERSION.store(current_version, Ordering::Relaxed);

        let pending = backend::bridge::take_pending_embed_layout_invalidations();
        let pending_count = pending.len();
        if !pending.is_empty() {
            let message_ids: std::collections::HashSet<String> = pending.into_iter().collect();
            crate::components::chat::canvas_renderer::global_layout_cache::clear_layouts_for_messages(
                &message_ids,
            );
        }

        crate::log_ui!(
            info,
            "Embed metadata version updated: {} -> {}, invalidated {} message layouts",
            last_version,
            current_version,
            pending_count
        );

        let chat_views: Vec<_> = self.chat_views.values().cloned().collect();
        for chat_view in chat_views {
            let canvas_list = chat_view.read(cx).canvas_list.clone();
            canvas_list.update(cx, |_, cx| cx.notify());
        }

        true
    }

    /// Check re-authentication status and show toast if needed.
    fn check_reauth_status(&mut self, cx: &mut Context<Self>) -> bool {
        if !backend::bridge::get_needs_reauth() {
            self.overlay_manager.update(cx, |overlays, _| {
                overlays.shown_reauth_toast = false;
            });
            return false;
        }

        let mut shown = false;
        self.overlay_manager.update(cx, |overlays, cx| {
            if !overlays.shown_reauth_toast {
                overlays.shown_reauth_toast = true;
                overlays.push_toast(
                    crate::components::overlays::Toast::warning("Authentication expired")
                        .with_detail("Click to re-authenticate, or continue in read-only mode")
                        .with_duration(10)
                        .with_on_click(backend::ui_actions::push_pending_open_login),
                    cx,
                );
                shown = true;
            }
        });
        shown
    }

    /// Process runtime settings that need app-owned entity mutations.
    fn process_pending_settings_sync(&mut self, cx: &mut Context<Self>) -> bool {
        let mut processed = false;

        let unified_input_enabled = backend::settings::get_unified_input_mode();
        if self.split_layout.read(cx).is_unified_input_mode() != unified_input_enabled {
            processed |= self.split_layout.update(cx, |layout, _| {
                layout.set_unified_input_mode(unified_input_enabled)
            });
        }

        let settings_invalidated =
            crate::components::chat::canvas_renderer::global_layout_cache::maybe_invalidate_for_settings_change();
        if settings_invalidated {
            crate::backend::metrics::record_ui_notify_source(
                "app.pending_settings_layout_invalidate",
            );
            processed = true;
        }

        processed
    }

    /// Process pending UI queues that need EchoApp-owned entities.
    fn process_pending_ui_work(&mut self, cx: &mut Context<Self>) -> bool {
        let mut processed = false;

        if backend::ui_actions::take_pending_open_login() {
            self.overlay_manager.update(cx, |overlays, cx| {
                overlays.open_login(cx);
            });
            processed = true;
        }

        for mut edit in backend::ui_actions::drain_pending_composer_edits() {
            if matches!(
                edit.action,
                backend::ui_actions::ComposerEditAction::PasteClipboard
            ) {
                match crate::utils::read_text(cx) {
                    Some(text) if !text.is_empty() => {
                        edit.action = backend::ui_actions::ComposerEditAction::Paste(text);
                    }
                    _ => continue,
                }
            }
            let target = {
                let app_state = self.app_state.read(cx);
                find_open_split_target(app_state, edit.split_id, "")
            };
            let Some((tab_index, path, channel)) = target else {
                continue;
            };
            processed = true;
            let is_paste = matches!(
                &edit.action,
                backend::ui_actions::ComposerEditAction::Paste(_)
            );
            if let backend::ui_actions::ComposerEditAction::Paste(text) = &edit.action {
                let offered = self.chat_input_renderer.update(cx, |renderer, cx| {
                    renderer.offer_multiline_paste(edit.split_id, &channel, text, cx)
                });
                if offered {
                    self.notify_split_leaf(edit.split_id, cx);
                    continue;
                }
            }

            let (current_text, copied_text) =
                self.chat_input_renderer.update(cx, |renderer, cx| {
                    use backend::ui_actions::ComposerEditAction;
                    let mut copied_text = None;
                    let changed = match edit.action {
                        ComposerEditAction::Undo => renderer.input_manager.undo(edit.split_id),
                        ComposerEditAction::Redo => renderer.input_manager.redo(edit.split_id),
                        ComposerEditAction::Paste(text) => {
                            renderer.insert_composer_paste(edit.split_id, &channel, &text, cx)
                        }
                        // Resolved into `Paste` when the edit was drained.
                        ComposerEditAction::PasteClipboard => false,
                        ComposerEditAction::DeleteSelection {
                            range,
                            expected_text,
                            copy,
                        } => {
                            let removed = renderer.input_manager.delete_exact_selection(
                                edit.split_id,
                                range,
                                &expected_text,
                            );
                            if copy {
                                copied_text = removed.clone();
                            }
                            removed.is_some()
                        }
                        ComposerEditAction::SelectAll => {
                            renderer.input_manager.select_all(edit.split_id)
                        }
                    };
                    if !changed {
                        return (None, None);
                    }
                    renderer.reset_composer_affinity(edit.split_id);
                    renderer.input_manager.end_history_navigation(edit.split_id);
                    let current_text = renderer
                        .input_manager
                        .input_texts
                        .get(&edit.split_id)
                        .cloned();
                    (current_text, copied_text)
                });
            if let Some(copied_text) = copied_text {
                crate::utils::copy_text(&copied_text);
            }
            if let Some(current_text) = current_text {
                self.notify_split_leaf(edit.split_id, cx);
                let is_active = tab_index == self.app_state.read(cx).active_tab_idx
                    && self.split_layout.read(cx).get_active_split_path() == path;
                if is_active {
                    self.update_autocomplete(&channel, &path, Some(&current_text), cx);
                }
            } else if is_paste {
                self.notify_split_leaf(edit.split_id, cx);
            }
        }

        for insertion in backend::drain_pending_message_composer_inserts() {
            processed = true;
            let target = {
                let app_state = self.app_state.read(cx);
                insertion.source_split_id.map_or_else(
                    || find_open_tab_channel_target(app_state, &insertion.channel),
                    |split_id| find_open_split_target(app_state, split_id, &insertion.channel),
                )
            };
            let Some((tab_index, path, channel)) = target else {
                crate::log_ui!(
                    info,
                    channel = %insertion.channel,
                    source_split_id = ?insertion.source_split_id,
                    "Skipped message-text insertion because its source or matching composer is no longer open"
                );
                continue;
            };

            self.activate_tab_split(tab_index, path.clone(), cx);
            let Some((_resolved_channel, split_id)) = self.split_target_for_path(&path, cx) else {
                continue;
            };

            let current_text = self.chat_input_renderer.update(cx, |renderer, _| {
                if !renderer
                    .input_manager
                    .insert_text_at_cursor(split_id, &insertion.text)
                {
                    return None;
                }
                renderer.reset_composer_affinity(split_id);
                renderer.input_manager.end_history_navigation(split_id);
                renderer.input_manager.input_texts.get(&split_id).cloned()
            });

            if let Some(current_text) = current_text {
                self.focus_split_input(split_id, cx);
                self.update_autocomplete(&channel, &path, Some(&current_text), cx);
                crate::log_ui!(
                    debug,
                    split_id = %split_id,
                    channel = %channel,
                    "Inserted message text into matching composer draft"
                );
            }
        }

        processed |= self.process_picker_actions(cx);

        for upload in backend::ui_actions::drain_confirmed_file_uploads() {
            processed = true;
            if upload.account == backend::ui_actions::composer_account()
                && self.composer_target_is_current(upload.split_id, &upload.channel, cx)
            {
                backend::upload_image_files_async(
                    upload.channel,
                    upload.split_id,
                    upload.paths,
                    upload.source,
                );
            } else {
                self.cancelled_upload_target(cx);
            }
        }

        for completed in backend::ui_actions::drain_pending_completed_sends() {
            processed = true;
            let split_id = completed.origin.split_id;
            let still_open = self.composer_target_is_current(split_id, &completed.channel, cx);
            let matched = self.chat_input_renderer.update(cx, |renderer, cx| {
                renderer.finish_send(
                    &completed.origin,
                    &completed.channel,
                    completed.notice.clone(),
                    cx,
                )
            });
            if matched && still_open {
                self.notify_split_leaf(split_id, cx);
            }
            self.advance_pasted_send(
                &completed.origin,
                matched && still_open && completed.notice.is_none(),
                completed.notice.as_deref().unwrap_or("Target changed"),
                cx,
            );
        }

        for failed in backend::ui_actions::drain_pending_failed_sends() {
            processed = true;
            let split_id = failed.origin.split_id;
            let notice = if failed.delivery_unknown {
                "Not confirmed — check chat".to_string()
            } else {
                backend::ui_actions::composer_send::rejection_reason("", &failed.error)
            };
            let matched = self.chat_input_renderer.update(cx, |renderer, cx| {
                renderer.finish_send(&failed.origin, &failed.channel, Some(notice), cx)
            });
            let still_open = self.composer_target_is_current(split_id, &failed.channel, cx);
            if !matched || !still_open {
                if failed.origin.account != backend::ui_actions::composer_account() {
                    let title = if failed.delivery_unknown {
                        "Previous-account send not confirmed"
                    } else {
                        "Previous-account message was not sent"
                    };
                    self.overlay_manager.update(cx, |manager, cx| {
                        manager.push_toast(
                            crate::components::overlays::Toast::warning(title).with_detail(
                                format!(
                                    "To #{}: {}. The text remains in that channel’s input history.",
                                    failed.channel,
                                    backend::ui_actions::composer_send::rejection_reason(
                                        "",
                                        &failed.error
                                    )
                                ),
                            ),
                            cx,
                        )
                    });
                    if still_open {
                        backend::bridge::publish_failed_send_row(
                            &failed.channel,
                            "Previous-account submission",
                            &failed.error,
                            true,
                            failed.delivery_unknown,
                        );
                    }
                }
                self.advance_pasted_send(&failed.origin, false, "Target changed", cx);
                continue;
            }
            let origin = failed.origin.clone();
            self.notify_split_leaf(split_id, cx);
            let restored = still_open
                && !failed.delivery_unknown
                && self.chat_input_renderer.update(cx, |renderer, _| {
                    let draft_empty = renderer
                        .input_manager
                        .input_texts
                        .get(&split_id)
                        .is_none_or(|text| text.is_empty());
                    if !draft_empty {
                        return false;
                    }
                    let restored = renderer
                        .input_manager
                        .insert_text_at_cursor(split_id, &failed.origin.text);
                    if restored {
                        renderer.reset_composer_affinity(split_id);
                    }
                    restored
                });
            if restored {
                if let Some(reply) = failed.origin.reply
                    && let Some(pane) = self.split_panes.get(&split_id).cloned()
                {
                    pane.update(cx, |pane, _| {
                        if pane.reply_to.is_none() {
                            pane.reply_to = Some(reply);
                        }
                    });
                }
                self.notify_split_leaf(split_id, cx);
            }
            backend::bridge::publish_failed_send_row(
                &failed.channel,
                failed.action,
                &failed.error,
                !restored,
                failed.delivery_unknown,
            );
            self.advance_pasted_send(
                &origin,
                false,
                if failed.delivery_unknown {
                    "Not confirmed — check chat"
                } else {
                    &failed.error
                },
                cx,
            );
        }

        for upload in backend::drain_pending_uploaded_links() {
            processed = true;
            let target = {
                let app_state = self.app_state.read(cx);
                find_open_split_target(app_state, upload.split_id, &upload.channel)
            };
            let Some((tab_index, path, channel)) = target else {
                crate::log_ui!(
                    info,
                    split_id = %upload.split_id,
                    channel = %upload.channel,
                    "Skipped uploaded-link insertion because the originating split no longer matches"
                );
                continue;
            };

            let inserted_text = format!("{} ", upload.url);
            let current_text = self.chat_input_renderer.update(cx, |renderer, _| {
                if !renderer
                    .input_manager
                    .insert_text_at_cursor(upload.split_id, &inserted_text)
                {
                    return None;
                }
                renderer.reset_composer_affinity(upload.split_id);
                renderer
                    .input_manager
                    .end_history_navigation(upload.split_id);
                renderer
                    .input_manager
                    .input_texts
                    .get(&upload.split_id)
                    .cloned()
            });

            if current_text.is_some() {
                let is_active_origin = tab_index == self.app_state.read(cx).active_tab_idx
                    && self.split_layout.read(cx).get_active_split_path() == path;
                if is_active_origin {
                    self.focus_split_input(upload.split_id, cx);
                    self.notify_split_leaf(upload.split_id, cx);
                    self.update_autocomplete(&channel, &path, current_text.as_deref(), cx);
                }
                crate::log_ui!(
                    debug,
                    split_id = %upload.split_id,
                    channel = %channel,
                    "Inserted uploaded image link into originating split draft"
                );
            }
        }

        for mention in backend::drain_pending_mentions() {
            processed = true;
            let active_path = self.split_layout.read(cx).get_active_split_path().to_vec();
            let fallback_channel = self
                .app_state
                .read(cx)
                .active_tab()
                .layout
                .get_channel_at_path(&active_path)
                .map(|s| s.to_string())
                .unwrap_or_else(|| mention.channel.clone());
            let target = {
                let app_state = self.app_state.read(cx);
                mention.source_split_id.map_or_else(
                    || find_open_tab_channel_target(app_state, &mention.channel),
                    |split_id| find_open_split_target(app_state, split_id, &mention.channel),
                )
            };
            if target.is_none()
                && (mention.source_split_id.is_some() || !mention.channel.trim().is_empty())
            {
                crate::log_ui!(
                    info,
                    source_split_id = ?mention.source_split_id,
                    channel = %mention.channel,
                    "Skipped mention insertion because its source split is stale"
                );
                continue;
            }
            let (target_channel, target_path) = if let Some((tab_index, path, channel)) = target {
                self.activate_tab_split(tab_index, path.clone(), cx);
                (channel, path)
            } else {
                (fallback_channel, active_path)
            };

            if !target_channel.is_empty() {
                let Some((_resolved_channel, split_id)) =
                    self.split_target_for_path(&target_path, cx)
                else {
                    continue;
                };
                let inserted = self.chat_input_renderer.update(cx, |renderer, _| {
                    let before = renderer.input_manager.input_texts.get(&split_id).cloned();
                    renderer
                        .input_manager
                        .insert_mention(split_id, &mention.username);
                    renderer.reset_composer_affinity(split_id);
                    renderer.input_manager.input_texts.get(&split_id).cloned() != before
                });
                if inserted {
                    self.focus_split_input(split_id, cx);
                    let current_text = self
                        .chat_input_renderer
                        .read(cx)
                        .input_manager
                        .input_texts
                        .get(&split_id)
                        .cloned();
                    self.update_autocomplete(
                        &target_channel,
                        &target_path,
                        current_text.as_deref(),
                        cx,
                    );
                    self.notify_split_leaf(split_id, cx);
                }
                crate::log_ui!(
                    debug,
                    "Inserted @mention '{}' into channel '{}'",
                    mention.username,
                    target_channel
                );
            }
        }

        if backend::take_pending_open_new_tab() {
            processed = true;
            self.create_new_tab(cx);
        }

        for open in backend::ui_actions::drain_pending_followed_channel_opens() {
            processed = true;
            self.open_followed_channel(open.login, open.fill_split, cx);
        }

        processed |= self.process_pending_chatterino_import(cx);

        for close in backend::ui_actions::drain_pending_bulk_tab_closes() {
            let scope = if close.to_right {
                TabCloseScope::ToRight
            } else {
                TabCloseScope::Others
            };
            processed |= self.close_tabs_by_id(&close.tab_id, scope, cx);
        }

        let closed_tab_generation = self.app_state.read(cx).closed_tab_generation();
        if closed_tab_generation != self.seen_closed_tab_generation {
            self.seen_closed_tab_generation = closed_tab_generation;
            processed = true;
            let count = self.app_state.read(cx).last_closed_tab_count();
            if count > 0 {
                let message = if count == 1 {
                    "Tab closed".to_string()
                } else {
                    format!("{count} tabs closed")
                };
                let requests = self.undo_closed_tab_requests.clone();
                let toast = crate::components::overlays::Toast::info(message).with_action(
                    "Undo",
                    move || {
                        requests
                            .lock()
                            .unwrap_or_else(|poisoned| poisoned.into_inner())
                            .push(closed_tab_generation);
                        backend::ui_actions::push_pending_reopen_closed_tab();
                    },
                );
                self.overlay_manager.update(cx, |manager, cx| {
                    manager.push_toast(toast, cx);
                });
            }
        }

        if backend::ui_actions::take_pending_reopen_closed_tab() {
            processed = true;
            let generations = std::mem::take(
                &mut *self
                    .undo_closed_tab_requests
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner()),
            );
            for generation in generations {
                let reopened = self.app_state.update(cx, |state, cx| {
                    state.reopen_closed_tab_batch(generation, cx)
                });
                self.finish_reopen_closed_tabs(reopened, cx);
            }
        }

        if backend::take_pending_open_whispers_split() {
            processed = true;
            let _ = self.open_or_focus_whispers_split(cx);
        }

        if backend::ui_actions::take_pending_refocus_active_input() {
            processed = true;
            self.refocus_active_input(cx);
        }

        for compose in backend::drain_pending_whisper_composes() {
            processed = true;
            let Some(target_path) = self.open_or_focus_whispers_split(cx) else {
                continue;
            };
            let Some((_resolved_channel, split_id)) = self.split_target_for_path(&target_path, cx)
            else {
                continue;
            };

            let inserted = self.chat_input_renderer.update(cx, |renderer, _| {
                let inserted = renderer
                    .input_manager
                    .compose_whisper(split_id, &compose.username);
                if inserted {
                    renderer.reset_composer_affinity(split_id);
                }
                inserted
            });

            if inserted {
                backend::mark_whisper_thread_read(&compose.username);
                self.app_state.update(cx, |state, _| {
                    state.refresh_sidebar_caches();
                });
                crate::log_ui!(
                    debug,
                    "Prepared whisper draft for '{}' in the Whispers split (requested from {:?})",
                    compose.username,
                    compose.channel
                );
            } else {
                let toast =
                    crate::components::overlays::Toast::warning("Whisper draft not inserted")
                        .with_detail("Clear the target input or existing whisper draft first.");
                self.overlay_manager.update(cx, |manager, cx| {
                    manager.push_toast(toast, cx);
                });
            }
        }

        for selection in backend::drain_pending_quick_switches() {
            processed = true;
            if self
                .app_state
                .read(cx)
                .tabs
                .get(selection.tab_index)
                .is_some()
            {
                self.activate_tab_split(selection.tab_index, selection.split_path, cx);
                self.refocus_active_input(cx);
            }
        }

        for reply in backend::drain_pending_replies() {
            processed = true;
            let active_path = self.split_layout.read(cx).get_active_split_path().to_vec();
            let target = {
                let app_state = self.app_state.read(cx);
                reply.source_split_id.map_or_else(
                    || {
                        find_open_tab_channel_target(app_state, &reply.channel)
                            .map(|(tab_index, path, _)| (tab_index, path))
                    },
                    |split_id| {
                        find_open_split_target(app_state, split_id, &reply.channel)
                            .map(|(tab_index, path, _)| (tab_index, path))
                    },
                )
            };
            if target.is_none()
                && (reply.source_split_id.is_some() || !reply.channel.trim().is_empty())
            {
                crate::log_ui!(
                    info,
                    source_split_id = ?reply.source_split_id,
                    channel = %reply.channel,
                    "Skipped reply activation because its source split is stale"
                );
                continue;
            }
            let target_path = if let Some((tab_index, path)) = target {
                self.activate_tab_split(tab_index, path.clone(), cx);
                path
            } else {
                active_path
            };
            let split_id = self
                .app_state
                .read(cx)
                .active_tab()
                .layout
                .get_split_id_at_path(&target_path);

            if let Some(split_id) = split_id {
                if let Some(pane) = self.split_panes.get(&split_id) {
                    pane.update(cx, |p, _| {
                        p.reply_to = Some(crate::state::ReplyState {
                            message_id: reply.message_id.clone(),
                            username: reply.sender.clone(),
                            preview: reply.content.clone(),
                        });
                    });
                }

                self.focus_split_input(split_id, cx);
                self.notify_split_leaf(split_id, cx);

                crate::log_ui!(
                    debug,
                    "Activated reply in split: channel='{}' msg_id='{}' display_name='{}'",
                    reply.channel,
                    reply.message_id,
                    reply.display_name
                );
            }
        }

        for view in backend::drain_pending_thread_views() {
            processed = true;
            self.overlay_manager.update(cx, |manager, cx| {
                manager.open_thread_viewer(
                    view.message_id.clone(),
                    view.channel.clone(),
                    view.x,
                    view.y,
                    cx,
                );
                cx.notify();
            });
        }

        for jump in backend::drain_pending_split_message_jumps() {
            processed = true;
            let target = {
                let app_state = self.app_state.read(cx);
                find_open_split_target(app_state, jump.split_id, &jump.channel)
            };

            let Some((tab_index, path, channel)) = target else {
                crate::log_ui!(
                    debug,
                    "Split-local message jump dropped because its source split is gone: split_id={} channel={}",
                    jump.split_id,
                    jump.channel
                );
                continue;
            };

            self.activate_tab_split(tab_index, path, cx);

            if let Some(chat_view) = self.chat_views.get(&jump.split_id).filter(|chat_view| {
                chat_view
                    .read(cx)
                    .channel
                    .trim_start_matches('#')
                    .eq_ignore_ascii_case(channel.trim_start_matches('#'))
            }) {
                let found = chat_view.update(cx, |view, cx| {
                    let found = view
                        .canvas_list
                        .update(cx, |list, cx| list.scroll_to_message(&jump.message_id, cx));
                    view.mark_dirty_for(ChatViewDirtyReason::SearchJump);
                    cx.notify();
                    found
                });
                if !found {
                    self.report_missing_jump_target(&channel, &jump.message_id, cx);
                }
            } else {
                crate::log_ui!(
                    debug,
                    "Split-local message jump waiting for a matching ChatView: split_id={} channel={}",
                    jump.split_id,
                    channel
                );
                backend::push_pending_split_message_jump(
                    jump.split_id,
                    jump.channel,
                    jump.message_id,
                );
            }
        }

        for jump in backend::drain_pending_search_jumps() {
            processed = true;
            let target = {
                let app_state = self.app_state.read(cx);
                find_open_tab_channel_target(app_state, &jump.channel)
            };

            let Some((tab_index, path, channel)) = target else {
                crate::log_ui!(
                    warn,
                    "Search result target channel is not visible in any open tab: {}",
                    jump.channel
                );
                self.overlay_manager.update(cx, |overlays, cx| {
                    overlays.toast_info(
                        format!(
                            "#{} is no longer open",
                            jump.channel.trim_start_matches('#')
                        ),
                        cx,
                    );
                });
                continue;
            };

            self.activate_tab_split(tab_index, path.clone(), cx);

            let Some((_resolved_channel, split_id)) = self.split_target_for_path(&path, cx) else {
                backend::push_pending_search_jump(channel.clone(), jump.message_id.clone());
                continue;
            };

            if let Some(chat_view) = self.chat_views.get(&split_id) {
                let found = chat_view.update(cx, |view, cx| {
                    let found = view
                        .canvas_list
                        .update(cx, |list, cx| list.scroll_to_message(&jump.message_id, cx));
                    view.mark_dirty_for(ChatViewDirtyReason::SearchJump);
                    cx.notify();
                    found
                });
                if !found {
                    self.report_missing_jump_target(&channel, &jump.message_id, cx);
                }
            } else {
                crate::log_ui!(
                    debug,
                    "Search result target split has no chat view yet, retrying after tab/split activation: channel='{}' path={:?}",
                    channel,
                    path
                );
                backend::push_pending_search_jump(channel.clone(), jump.message_id.clone());
            }
        }

        processed
    }

    /// Tell the user a jump target is gone. A message still in the channel
    /// buffer is only not laid out yet, so that case stays silent.
    fn report_missing_jump_target(&self, channel: &str, message_id: &str, cx: &mut Context<Self>) {
        let buffered = self
            .app_state
            .read(cx)
            .chat_backend
            .read(cx)
            .get_channel(channel)
            .is_some_and(|chat| {
                chat.read(cx)
                    .messages
                    .iter()
                    .any(|message| message.id.as_ref() == message_id)
            });
        if buffered {
            return;
        }
        self.overlay_manager.update(cx, |overlays, cx| {
            overlays.toast_info("Message is no longer in scrollback", cx);
        });
    }

    /// Update window dimensions and invalidate layout caches if changed
    fn update_layout_caches(&mut self, window: &Window, cx: &mut Context<Self>) {
        let viewport_size = window.viewport_size();
        let window_width: f32 = viewport_size.width.into();
        let window_maximized = window.is_maximized();

        if !crate::benchmark_ui_fixture::should_suppress_persistence() {
            if self.last_persisted_window_maximized != window_maximized {
                backend::settings::set_main_window_maximized(window_maximized);
                self.last_persisted_window_maximized = window_maximized;
            }

            if !window_maximized {
                crate::utils::persist_window_bounds(
                    window,
                    &mut self.last_persisted_window_position,
                    &mut self.last_persisted_window_size,
                    backend::settings::set_main_window_position,
                    backend::settings::set_main_window_size,
                );
            }
        }

        let current_split_count = {
            let app_state = self.app_state.read(cx);
            app_state.active_tab().layout.count_leaves()
        };

        self.split_layout.update(cx, |layout, _| {
            layout.track_window_layout(window_width, current_split_count);
        });
    }

    fn desired_window_title() -> String {
        let mention_count = backend::get_unread_global_mention_count();
        if backend::settings::get_show_mention_in_title() && mention_count > 0 {
            let label = if mention_count > 99 {
                "99+".to_string()
            } else {
                mention_count.to_string()
            };
            format!("({}) Echo", label)
        } else {
            "Echo".to_string()
        }
    }

    fn update_window_title(&mut self, window: &mut Window) {
        let title = Self::desired_window_title();
        if self.last_window_title != title {
            window.set_window_title(&title);
            self.last_window_title = title;
        }
    }

    /// Chrome menus take keyboard focus while open so their keys never reach
    /// the input behind them; closing one hands focus back to the active input
    /// unless a menu action already moved it.
    fn sync_chrome_menu_focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let tab_menu_open = {
            let tab_bar = self.tab_bar_renderer.read(cx);
            tab_bar.is_context_menu_open() || tab_bar.is_overflow_open()
        };
        let (menu_open, menu_focus) = self.overlay_manager.update(cx, |overlays, _| {
            overlays.sync_tab_menu_layer(tab_menu_open);
            (
                tab_menu_open
                    || overlays.account_switcher_open
                    || overlays.open_settings_dropdown.is_some(),
                overlays.menu_focus.clone(),
            )
        });
        let was_open = std::mem::replace(&mut self.last_chrome_menu_open, menu_open);
        if menu_open {
            if !was_open && !menu_focus.is_focused(window) {
                window.focus(&menu_focus, cx);
            }
        } else if was_open && menu_focus.is_focused(window) {
            self.refocus_active_input(cx);
        }
    }

    /// The confirmation modal owns keyboard focus while open, so Enter confirms
    /// instead of sending the draft behind it.
    fn sync_confirmation_focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (open, focus) = {
            let modal = self.overlay_manager.read(cx).confirmation_modal.read(cx);
            (modal.state.is_open, modal.focus_handle.clone())
        };
        let was_open = std::mem::replace(&mut self.last_confirmation_open, open);
        if open {
            if !was_open && !focus.is_focused(window) {
                window.focus(&focus, cx);
            }
        } else if was_open && (focus.is_focused(window) || window.focused(cx).is_none()) {
            if self.last_chrome_menu_open {
                let menu_focus = self.overlay_manager.read(cx).menu_focus.clone();
                window.focus(&menu_focus, cx);
            } else {
                self.refocus_active_input(cx);
            }
        }
    }

    /// Focus, highlight and Escape handling shared by the chrome menus. Escape
    /// dismisses through the overlay escape stack, where each open menu is
    /// registered.
    fn chrome_menu_host(&self, cx: &Context<Self>) -> MenuHost {
        let overlays = self.overlay_manager.read(cx);
        let overlay_manager = self.overlay_manager.clone();
        let app = cx.entity();
        MenuHost {
            focus: overlays.menu_focus.clone(),
            nav: overlays.menu_nav.clone(),
            on_close: Rc::new(move |_window: &mut Window, cx: &mut App| {
                overlay_manager.update(cx, |overlays, cx| {
                    overlays.dismiss_top_escape_layer(cx);
                });
                app.update(cx, |_, cx| cx.notify());
            }),
        }
    }

    /// Render all overlays (modals, dropdowns, menus)
    /// PERF: Only renders overlays that are actually open to avoid unnecessary allocations
    fn render_all_overlays(
        &mut self,
        window: &mut Window,
        chrome_height: f32,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        self.sync_chrome_menu_focus(window, cx);
        self.sync_confirmation_focus(window, cx);
        let overlay_mgr = self.overlay_manager.read(cx);
        let settings_open = overlay_mgr.settings_modal.read(cx).is_open;
        let login_open = overlay_mgr.login_modal.read(cx).is_open;
        let search_open = overlay_mgr.search_modal.read(cx).is_expanded();
        let mod_log_open = overlay_mgr.mod_log_viewer.read(cx).state.is_open;
        let emote_picker_open = overlay_mgr.emote_picker.read(cx).state.is_open;
        let user_card_open = overlay_mgr.user_card.read(cx).is_open();
        let chatter_list_open = overlay_mgr.chatter_list.read(cx).state.is_open;
        let context_menu_open = overlay_mgr.context_menu.read(cx).state.is_open;
        let account_dropdown_open = overlay_mgr.account_switcher_open;
        let thread_viewer_open = overlay_mgr.thread_viewer.read(cx).is_open();
        let hotkey_cheat_sheet_open = overlay_mgr.hotkey_cheat_sheet.read(cx).is_open();
        let confirmation_open = overlay_mgr.confirmation_modal.read(cx).state.is_open;
        let quick_switcher_open = overlay_mgr.quick_switcher.read(cx).is_open;
        let metrics_open = overlay_mgr.metrics_overlay.read(cx).is_open();

        let mut overlays = Vec::with_capacity(4);

        let app_state = self.app_state.clone();
        let menu_host = self.chrome_menu_host(cx);
        if self.tab_bar_renderer.read(cx).is_context_menu_open() {
            overlays.push(self.tab_bar_renderer.update(cx, |r, cx| {
                r.render_context_menu_overlay(app_state.clone(), &menu_host, window, cx)
            }));
        }
        if self.tab_bar_renderer.read(cx).is_overflow_open() {
            overlays.push(self.tab_bar_renderer.update(cx, |r, cx| {
                r.render_overflow_dropdown_overlay(app_state, &menu_host, window, cx)
            }));
        }

        if settings_open {
            overlays.push(self.render_settings_modal(cx).into_any_element());
        }
        if login_open {
            overlays.push(self.render_login_modal(window, cx).into_any_element());
        }
        if search_open {
            overlays.push(self.render_search_modal(cx).into_any_element());
        }
        if mod_log_open {
            overlays.push(self.render_mod_log_modal(cx).into_any_element());
        }
        if emote_picker_open {
            overlays.push(self.render_emote_picker(window, cx).into_any_element());
        }
        if thread_viewer_open {
            overlays.push(self.render_thread_viewer(cx).into_any_element());
        }
        if user_card_open {
            overlays.push(self.render_user_card(cx).into_any_element());
        }
        if chatter_list_open {
            overlays.push(self.render_chatter_list(cx).into_any_element());
        }
        if context_menu_open {
            overlays.push(self.render_context_menu(cx).into_any_element());
        }
        if account_dropdown_open {
            overlays.push(
                crate::components::top_bar::render_account_dropdown(
                    &self.overlay_manager,
                    &menu_host,
                    chrome_height,
                    window,
                    cx,
                )
                .into_any_element(),
            );
        }
        if hotkey_cheat_sheet_open {
            let sheet = self.overlay_manager.read(cx).hotkey_cheat_sheet.clone();
            overlays.push(sheet.into_any_element());
        }
        if confirmation_open {
            overlays.push(self.render_confirmation_modal(cx).into_any_element());
        }
        if quick_switcher_open {
            let qs = self.overlay_manager.read(cx).quick_switcher.clone();
            overlays.push(qs.into_any_element());
        }

        if metrics_open {
            overlays.push(self.render_metrics_overlay(cx).into_any_element());
        }

        overlays.push(
            self.render_settings_dropdown_backdrop(chrome_height, window, cx)
                .into_any_element(),
        );
        overlays.push(self.render_toast_manager(cx).into_any_element());
        overlays.push(self.render_drag_ghost(cx).into_any_element());
        crate::components::overlays::ensure_global_tooltip_animation_waker(
            gpui::Window::window_handle(window),
            cx,
        );
        if let Some(global_tooltip) = crate::components::overlays::render_global_tooltip_overlay(
            gpui::Window::window_handle(window),
            window.viewport_size(),
            window.mouse_position(),
        ) {
            overlays.push(global_tooltip);
        }

        overlays
    }

    /// Everything the one unified composer needs about the split it is typing
    /// into, captured in a single pass.
    ///
    /// Channel, `SplitId`, reply state, ROOMSTATE and the surface metrics are
    /// resolved together so the target label, the reply bar, the room-state
    /// rail and the reserved height can never describe two different splits for
    /// a frame while focus moves. No active split means no snapshot, and
    /// therefore no room-state indicators.
    fn unified_input_target(&self, cx: &Context<Self>) -> Option<UnifiedInputTarget> {
        if !self.split_layout.read(cx).is_unified_input_mode() {
            return None;
        }
        let path = self.split_layout.read(cx).get_active_split_path().to_vec();
        let (channel, split_id) = self.split_target_for_path(&path, cx)?;
        let reply_state = self
            .split_panes
            .get(&split_id)
            .and_then(|pane| pane.read(cx).reply_to.clone());
        let room_state = crate::backend::irc::room_state::get_room_state(&channel);
        let pane_width = self.split_layout.read(cx).pane_width_for_path(&path);
        let metrics = self
            .chat_input_renderer
            .read(cx)
            .input_surface_metrics_for_split(split_id, reply_state.is_some());

        Some(UnifiedInputTarget {
            channel,
            path,
            split_id,
            reply_state,
            room_state,
            pane_width,
            metrics,
        })
    }

    /// Render unified input at bottom when enabled
    fn render_unified_input(
        &mut self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let target = self.unified_input_target(cx)?;
        let app_entity = cx.entity().clone();
        let overlay_manager = self.overlay_manager.clone();
        let input_focus = self.input_focus.clone();

        let input_elem = self.chat_input_renderer.update(cx, |renderer, cx| {
            renderer.render(
                &target.channel,
                &target.path,
                &input_focus,
                true,
                target.split_id,
                target.reply_state,
                target.room_state.as_ref(),
                target.pane_width,
                app_entity,
                overlay_manager,
                window,
                cx,
            )
        });

        Some(
            div()
                .id("unified-input-container")
                .w_full()
                .flex_shrink_0()
                .bg(colors::bg_surface())
                .flex()
                .flex_col()
                .child(input_elem)
                .into_any_element(),
        )
    }

    /// Render main content area with split tree
    fn render_main_content_section(
        &mut self,
        layout: &SplitNode,
        highlights_enabled: bool,
        window_width: f32,
        window_height: f32,
        chrome_height: f32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let unified_input_height = if self.split_layout.read(cx).is_unified_input_mode() {
            self.unified_input_target(cx)
                .map(|target| target.metrics.total_height)
                .unwrap_or(config::CHAT_INPUT_HEIGHT_PX)
        } else {
            0.0
        };
        let content_height = (window_height - chrome_height - unified_input_height).max(0.0);

        div()
            .flex_1()
            .min_h_0()
            .w_full()
            .overflow_hidden()
            .child(self.render_main_content_fast(
                layout,
                window_width,
                content_height,
                highlights_enabled,
                window,
                cx,
            ))
    }

    /// Register all keyboard and mouse event handlers on the root div
    fn register_event_handlers(
        &mut self,
        element: Stateful<Div>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        crate::event_handlers::register_event_handlers(self, element, window, cx)
    }

    /// Startup marker for the first root render whose active split has chat
    /// rows to lay out.
    fn record_first_chat_view_usable(&self, cx: &App) {
        static RECORDED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
        if RECORDED.load(std::sync::atomic::Ordering::Relaxed) {
            return;
        }
        let state = self.app_state.read(cx);
        let has_rows = state.active_tab().active_channel().is_some_and(|channel| {
            state
                .chat_backend
                .read(cx)
                .get_channel(&channel)
                .is_some_and(|chat| !chat.read(cx).messages.is_empty())
        });
        if has_rows {
            RECORDED.store(true, std::sync::atomic::Ordering::Relaxed);
            crate::startup_metrics::record("first_chat_view_usable");
        }
    }

    /// Save tabs to disk
    pub fn save_tabs(&self, cx: &mut Context<Self>) {
        if crate::benchmark_ui_fixture::should_suppress_persistence() {
            return;
        }
        config::queue_save_tabs(self.app_state.read(cx).tabs_for_persistence());
    }

    /// PERF: Optimized main content that takes pre-extracted layout
    fn render_main_content_fast(
        &mut self,
        layout: &SplitNode,
        available_width: f32,
        available_height: f32,
        highlights_enabled: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let activity_open = self.app_state.read(cx).activity_open();

        let activity_is_overlay = activity_open && available_width < ACTIVITY_OVERLAY_BREAKPOINT_PX;
        let sidebar_width = if activity_open {
            if activity_is_overlay {
                available_width.min(config::SIDEBAR_WIDTH_PX)
            } else {
                activity_sidebar_width(available_width)
            }
        } else {
            0.0
        };
        let content_width = if activity_is_overlay {
            available_width
        } else {
            (available_width - sidebar_width).max(0.0)
        };

        let layout_locked = self.app_state.read(cx).active_tab().is_locked;
        let split_ctx = {
            let layout_state = self.split_layout.read(cx);
            let drop = layout_state.drop_target().map(|t| {
                (
                    crate::managers::SplitLayoutManager::path_key(&t.target_path),
                    t.zone,
                )
            });
            SplitRenderContext {
                is_dragging: layout_state.is_dragging_pane(),
                add_split_mode: layout_state.is_add_split_mode_active() && !layout_locked,
                resize_mode_active: layout_state.is_drag_mode_active() && !layout_locked,
                drop_target: drop,
                hovered: layout_state.hovered_split().cloned(),
            }
        };

        self.split_layout
            .update(cx, |layout, _| layout.begin_hit_area_pass());

        let header_height = main_split_header_height(backend::settings::get_compact_split_header());
        let chat_height = crate::components::chat::canvas_renderer::layout::line_height();
        self.split_layout.update(cx, |manager, cx| {
            let split_input = !manager.is_unified_input_mode();
            let active_split = layout.get_split_id_at_path(manager.get_active_split_path());
            manager.update_split_minimums(layout, |split_id, channel, header_collapsed| {
                let header = if header_collapsed { 0.0 } else { header_height };
                let chrome = self.split_vertical_chrome(
                    split_id,
                    channel,
                    header,
                    split_input,
                    split_id.is_some() && split_id == active_split,
                    120.0,
                    cx,
                );
                let context_slack = if split_input {
                    (config::CHAT_INPUT_CONTEXT_HEIGHT_PX - chrome.input).max(0.0)
                } else {
                    0.0
                };
                (120.0, chrome.height() + context_slack + chat_height)
            });
        });

        let split_start = crate::fps_profiler::phase_timer();
        let mut path = Vec::with_capacity(8);
        let split_tree = self.render_split_tree(
            layout,
            &mut path,
            content_width,
            available_height,
            highlights_enabled,
            &split_ctx,
            window,
            cx,
        );
        crate::fps_profiler::record_phase_since("split_tree", split_start);

        let activity_sidebar = if activity_open {
            let sidebar = self
                .render_activity_sidebar(sidebar_width, cx)
                .into_any_element();
            if activity_is_overlay {
                Some(
                    div()
                        .id("activity-sidebar-overlay")
                        .absolute()
                        .top_0()
                        .right_0()
                        .bottom_0()
                        .w(px(sidebar_width))
                        .shadow_lg()
                        .occlude()
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .on_mouse_down(MouseButton::Right, |_, _, cx| cx.stop_propagation())
                        .on_mouse_down(MouseButton::Middle, |_, _, cx| cx.stop_propagation())
                        .on_mouse_up(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .on_mouse_up(MouseButton::Right, |_, _, cx| cx.stop_propagation())
                        .on_mouse_up(MouseButton::Middle, |_, _, cx| cx.stop_propagation())
                        .on_mouse_move(|_, _, cx| cx.stop_propagation())
                        .on_scroll(|_, _, cx| cx.stop_propagation())
                        .child(sidebar)
                        .into_any_element(),
                )
            } else {
                Some(sidebar)
            }
        } else {
            None
        };

        div()
            .id("main-content")
            .w(px(available_width))
            .h(px(available_height))
            .relative()
            .flex()
            .flex_row()
            .gap_0()
            .bg(colors::bg_void())
            .child(split_tree)
            .children(activity_sidebar)
    }

    fn render_split_tree(
        &mut self,
        node: &SplitNode,
        path: &mut Vec<usize>,
        available_width: f32,
        available_height: f32,
        highlights_enabled: bool,
        split_ctx: &SplitRenderContext,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let available_width = available_width.max(0.0);
        let available_height = available_height.max(0.0);

        let path_key = SplitLayoutManager::path_key(path);
        self.split_layout.update(cx, |layout, _| {
            layout.register_split_size(path_key.clone(), available_width, available_height);
        });

        match node {
            SplitNode::Leaf {
                split_id,
                channel,
                header_collapsed,
                ..
            } => {
                let bounds_split_id = *split_id;
                let ch = crate::backend::browser_watching::resolve_layout_channel(channel);
                let drop_zone = split_ctx
                    .drop_target
                    .as_ref()
                    .filter(|(target_key, _)| split_ctx.is_dragging && target_key == &path_key)
                    .map(|(_, zone)| *zone);
                let show_add_split_overlay = split_ctx.add_split_mode
                    && !split_ctx.is_dragging
                    && split_ctx.hovered.as_ref() == Some(&path_key);

                // Drag and Ctrl+Alt affordances paint as an absolute sibling over
                // the pane, so the pane itself stays on its cached render path.
                let overlay = if drop_zone.is_some() {
                    Some(
                        div()
                            .absolute()
                            .inset_0()
                            .border_1()
                            .border_color(colors::accent())
                            .child(self.render_drop_zone_overlay(
                                drop_zone,
                                available_width,
                                available_height,
                                cx,
                            ))
                            .into_any_element(),
                    )
                } else if show_add_split_overlay {
                    Some(
                        self.render_add_split_buttons(
                            path.clone(),
                            available_width,
                            available_height,
                            cx,
                        )
                        .into_any_element(),
                    )
                } else {
                    None
                };

                if let (Some(split_id), Some(ch)) = (*split_id, ch.as_deref())
                    && !is_placeholder(ch)
                {
                    return self.render_cached_split_leaf(
                        split_id,
                        ch,
                        path.clone(),
                        available_width,
                        available_height,
                        highlights_enabled,
                        *header_collapsed,
                        overlay,
                        window,
                        cx,
                    );
                }

                let pane = if let Some(ch) = ch.as_deref() {
                    self.render_chat_pane(
                        ch,
                        path.clone(),
                        available_width,
                        available_height,
                        highlights_enabled,
                        *header_collapsed,
                        window,
                        cx,
                    )
                } else {
                    crate::components::splits::render_watching_pane(
                        self,
                        path.clone(),
                        available_width,
                        available_height,
                        window,
                        cx,
                    )
                };
                let entity = cx.entity().clone();

                div()
                    .id(("split-leaf", path_key.as_u64()))
                    .w(px(available_width))
                    .h(px(available_height))
                    .min_h_0()
                    .min_w_0()
                    .flex_shrink_0()
                    .overflow_hidden()
                    .relative()
                    .child(pane)
                    .children(overlay)
                    .child(
                        canvas(
                            move |bounds, _window, cx| {
                                entity.update(cx, |app, cx| {
                                    let hover_changed = app.split_layout.update(cx, |layout, _| {
                                        layout.register_leaf_bounds(
                                            bounds_split_id,
                                            path_key,
                                            bounds,
                                        );
                                        layout.recompute_hovered_split()
                                            && layout.is_add_split_mode_active()
                                    });
                                    if hover_changed {
                                        cx.notify();
                                    }
                                });
                            },
                            |_bounds, _: (), _window, _cx| {},
                        )
                        .absolute()
                        .inset_0(),
                    )
                    .into_any_element()
            }
            SplitNode::Split {
                orientation,
                children,
                ..
            } => {
                let split_arm_start = crate::fps_profiler::phase_timer();
                let child_count = children.len();
                let handle_count = child_count.saturating_sub(1);
                use crate::managers::{HANDLE_HIT_PADDING, HANDLE_HIT_SIZE, HANDLE_VISUAL_SIZE};
                let resize_mode_active = split_ctx.resize_mode_active;
                let parent_key = SplitLayoutManager::path_key(path);

                let (usable_width, usable_height) = match orientation {
                    SplitOrientation::Vertical => (
                        (available_width - (handle_count as f32 * HANDLE_VISUAL_SIZE)).max(0.0),
                        available_height,
                    ),
                    SplitOrientation::Horizontal => (
                        available_width,
                        (available_height - (handle_count as f32 * HANDLE_VISUAL_SIZE)).max(0.0),
                    ),
                };

                let minimums = self.split_layout.read(cx).child_minimum_extents(
                    path,
                    child_count,
                    orientation,
                );
                let extent = match orientation {
                    SplitOrientation::Vertical => usable_width,
                    SplitOrientation::Horizontal => usable_height,
                };
                let ratios = node.ratios_with_minimums(extent, &minimums);
                let grid_band_split = if resize_mode_active {
                    node.grid_band_split_ratio().and_then(|_| {
                        let mut band_ratios = Vec::with_capacity(2);
                        for (idx, child) in children.iter().enumerate() {
                            let SplitNode::Split {
                                orientation: child_orientation,
                                children,
                                ..
                            } = child
                            else {
                                return None;
                            };
                            let mut child_path = path.clone();
                            child_path.push(idx);
                            let minimums = self.split_layout.read(cx).child_minimum_extents(
                                &child_path,
                                children.len(),
                                child_orientation,
                            );
                            let child_extent = match child_orientation {
                                SplitOrientation::Vertical => available_width,
                                SplitOrientation::Horizontal => available_height,
                            } - children.len().saturating_sub(1) as f32
                                * HANDLE_VISUAL_SIZE;
                            band_ratios.push(
                                child.ratios_with_minimums(child_extent.max(0.0), &minimums)[0],
                            );
                        }
                        ((band_ratios[0] - band_ratios[1]).abs() < 0.004).then_some(band_ratios[0])
                    })
                } else {
                    None
                };

                let mut elements: Vec<AnyElement> = Vec::with_capacity(child_count + handle_count);

                for (idx, child) in children.iter().enumerate() {
                    path.push(idx);

                    let ratio = ratios[idx];

                    let (child_width, child_height) = match orientation {
                        SplitOrientation::Vertical => {
                            ((usable_width * ratio).max(0.0), usable_height)
                        }
                        SplitOrientation::Horizontal => {
                            (usable_width, (usable_height * ratio).max(0.0))
                        }
                    };

                    elements.push(self.render_split_tree(
                        child,
                        path,
                        child_width,
                        child_height,
                        highlights_enabled,
                        split_ctx,
                        window,
                        cx,
                    ));
                    path.pop();

                    if idx < children.len() - 1 {
                        let handle_id = {
                            use std::hash::{Hash, Hasher};
                            let mut hasher = ahash::AHasher::default();
                            parent_key.as_u64().hash(&mut hasher);
                            idx.hash(&mut hasher);
                            hasher.finish()
                        };

                        let handle_color = if resize_mode_active {
                            colors::accent()
                        } else {
                            colors::border()
                        };
                        let handle = if resize_mode_active {
                            let handle_path = path.clone();
                            let handle_orientation = orientation.clone();
                            let handle_idx = idx;
                            let container_size = match orientation {
                                SplitOrientation::Vertical => usable_width,
                                SplitOrientation::Horizontal => usable_height,
                            };
                            let divider_span = match orientation {
                                SplitOrientation::Vertical => available_height,
                                SplitOrientation::Horizontal => available_width,
                            };
                            let cursor = match orientation {
                                SplitOrientation::Vertical => CursorStyle::ResizeLeftRight,
                                SplitOrientation::Horizontal => CursorStyle::ResizeUpDown,
                            };
                            let tint = colors::accent().opacity(0.22);

                            let band_spans: Vec<(f32, f32, Option<usize>)> = match grid_band_split {
                                Some(ratio) => {
                                    let first = (divider_span * ratio).clamp(0.0, divider_span);
                                    vec![
                                        (0.0, first, Some(0)),
                                        (first, divider_span - first, Some(1)),
                                    ]
                                }
                                None => vec![(0.0, divider_span, None)],
                            };

                            let hit_bands: Vec<AnyElement> = band_spans
                                .into_iter()
                                .map(|(offset, extent, band)| {
                                    let band_path = handle_path.clone();
                                    let band_orientation = handle_orientation.clone();
                                    let band_div = match orientation {
                                        SplitOrientation::Vertical => div()
                                            .absolute()
                                            .left(px(-HANDLE_HIT_PADDING))
                                            .top(px(offset))
                                            .w(px(HANDLE_HIT_SIZE))
                                            .h(px(extent)),
                                        SplitOrientation::Horizontal => div()
                                            .absolute()
                                            .top(px(-HANDLE_HIT_PADDING))
                                            .left(px(offset))
                                            .h(px(HANDLE_HIT_SIZE))
                                            .w(px(extent)),
                                    };

                                    let band_div = band_div
                                        .id(SharedString::from(format!(
                                            "resize-band-{handle_id}-{}",
                                            band.unwrap_or(0)
                                        )))
                                        .bg(tint)
                                        .hover(|style| style.bg(colors::accent().opacity(0.38)))
                                        .cursor(cursor)
                                        .occlude()
                                        .on_mouse_down(
                                            MouseButton::Left,
                                            cx.listener(
                                                move |this, event: &MouseDownEvent, _window, cx| {
                                                    cx.stop_propagation();
                                                    if event.click_count >= 2 {
                                                        this.reset_split_ratios(&band_path, cx);
                                                        return;
                                                    }
                                                    match band {
                                                        Some(band_idx) => this
                                                            .start_grid_divider_drag(
                                                                band_path.clone(),
                                                                band_idx,
                                                                band_orientation.clone(),
                                                                event.position,
                                                                container_size,
                                                                cx,
                                                            ),
                                                        None => this.start_split_drag(
                                                            band_path.clone(),
                                                            handle_idx,
                                                            band_orientation.clone(),
                                                            event.position,
                                                            container_size,
                                                            cx,
                                                        ),
                                                    }
                                                },
                                            ),
                                        )
                                        .on_mouse_up(
                                            MouseButton::Left,
                                            cx.listener(|this, _, _, cx| {
                                                this.end_split_drag_from_pointer(cx);
                                            }),
                                        )
                                        .on_mouse_up_out(
                                            MouseButton::Left,
                                            cx.listener(|this, _, _, cx| {
                                                this.end_split_drag_from_pointer(cx);
                                            }),
                                        );
                                    crate::components::splits::accessible_split_divider(
                                        band_div,
                                        self,
                                        &handle_path,
                                        handle_idx,
                                        band,
                                        cx,
                                    )
                                    .into_any_element()
                                })
                                .collect();

                            match orientation {
                                SplitOrientation::Vertical => div()
                                    .id(("drag-handle", handle_id))
                                    .w(px(HANDLE_VISUAL_SIZE))
                                    .h(px(available_height))
                                    .flex_shrink_0()
                                    .bg(handle_color)
                                    .relative()
                                    .children(hit_bands)
                                    .into_any_element(),
                                SplitOrientation::Horizontal => div()
                                    .id(("drag-handle", handle_id))
                                    .h(px(HANDLE_VISUAL_SIZE))
                                    .w(px(available_width))
                                    .flex_shrink_0()
                                    .bg(handle_color)
                                    .relative()
                                    .children(hit_bands)
                                    .into_any_element(),
                            }
                        } else {
                            let divider = match orientation {
                                SplitOrientation::Vertical => div()
                                    .id(("drag-handle", handle_id))
                                    .w(px(HANDLE_VISUAL_SIZE))
                                    .h(px(available_height))
                                    .flex_shrink_0()
                                    .bg(handle_color),
                                SplitOrientation::Horizontal => div()
                                    .id(("drag-handle", handle_id))
                                    .h(px(HANDLE_VISUAL_SIZE))
                                    .w(px(available_width))
                                    .flex_shrink_0()
                                    .bg(handle_color),
                            };
                            crate::components::splits::accessible_split_divider(
                                divider, self, path, idx, None, cx,
                            )
                            .into_any_element()
                        };
                        elements.push(handle);
                    }
                }

                crate::fps_profiler::record_phase_since("split_handles", split_arm_start);

                match orientation {
                    SplitOrientation::Vertical => div()
                        .flex()
                        .flex_row()
                        .gap_0()
                        .w(px(available_width))
                        .h(px(available_height))
                        .min_h_0()
                        .min_w_0()
                        .flex_shrink_0()
                        .children(elements)
                        .into_any_element(),
                    SplitOrientation::Horizontal => div()
                        .flex()
                        .flex_col()
                        .gap_0()
                        .w(px(available_width))
                        .h(px(available_height))
                        .min_h_0()
                        .min_w_0()
                        .flex_shrink_0()
                        .children(elements)
                        .into_any_element(),
                }
            }
        }
    }

    fn render_activity_sidebar(&mut self, width: f32, cx: &mut Context<Self>) -> AnyElement {
        let (panel, mentions, whispers) = {
            let state = self.app_state.read(cx);
            (
                state.activity_panel,
                state.cached_mentions.clone(),
                state.cached_whispers.clone(),
            )
        };
        let mention_count = backend::get_unread_global_mention_count();
        let unread_whispers = whispers.iter().fold(0u32, |total, thread| {
            total.saturating_add(thread.unread_count)
        });
        let events = match panel.section {
            ActivitySection::All | ActivitySection::Events => backend::get_recent_twitch_events(50),
            ActivitySection::Mentions | ActivitySection::Whispers => Vec::new(),
        };
        let event_count = backend::get_unread_twitch_event_count();
        let unread_activity_count = mention_count
            .saturating_add(unread_whispers as usize)
            .saturating_add(event_count);

        let content = match panel.section {
            ActivitySection::All => {
                let app_state_for_read = self.app_state.clone();
                let app_state_for_clear = self.app_state.clone();
                crate::components::sidebars::render_combined_activity_content(
                    mentions,
                    whispers,
                    events,
                    unread_activity_count,
                    move |cx: &mut Context<Self>| {
                        backend::mark_all_activity_read();
                        app_state_for_read.update(cx, |state, cx| state.refresh_activity(cx));
                    },
                    move |cx: &mut Context<Self>| {
                        backend::clear_all_activity();
                        app_state_for_clear.update(cx, |state, cx| state.refresh_activity(cx));
                    },
                    cx,
                )
                .into_any_element()
            }
            ActivitySection::Mentions => {
                let app_state = self.app_state.clone();
                crate::components::sidebars::render_mentions_activity_content(
                    mentions,
                    move |cx: &mut Context<Self>| {
                        backend::clear_global_mentions();
                        app_state.update(cx, |state, cx| state.refresh_activity(cx));
                    },
                    cx,
                )
                .into_any_element()
            }
            ActivitySection::Whispers => {
                let app_state = self.app_state.clone();
                crate::components::sidebars::render_whispers_activity_content(
                    whispers,
                    move |cx: &mut Context<Self>| {
                        backend::mark_all_whisper_threads_read();
                        app_state.update(cx, |state, cx| state.refresh_activity(cx));
                    },
                    cx,
                )
                .into_any_element()
            }
            ActivitySection::Events => {
                self.render_activity_events_content(panel.event_filter, events, cx)
            }
        };

        let app_state_for_select = self.app_state.clone();
        let app_state_for_close = self.app_state.clone();
        crate::components::sidebars::render_activity_sidebar(
            width,
            panel.section,
            mention_count,
            unread_whispers,
            event_count,
            content,
            move |section, cx: &mut Context<Self>| {
                app_state_for_select
                    .update(cx, |state, cx| state.select_activity_section(section, cx));
            },
            move |cx: &mut Context<Self>| {
                app_state_for_close.update(cx, |state, cx| state.close_activity(cx));
            },
            cx,
        )
        .into_any_element()
    }

    /// Clear unread Activity attention using the visible state of mounted chat canvases.
    ///
    /// Runs at UI-poll cadence, so it is gated twice: nothing can be cleared while
    /// both unread sets are empty, and an unchanged viewport cannot clear anything
    /// the pass that produced the same key did not already clear. The unread counts
    /// are part of the key so newly arrived attention always re-harvests.
    fn mark_observed_activity_read(&mut self, cx: &Context<Self>) -> bool {
        let unread_mentions = backend::get_unread_global_mention_count();
        let unread_events = backend::get_unread_twitch_event_count();
        if unread_mentions == 0 && unread_events == 0 {
            self.activity_read_visibility_key = None;
            return false;
        }

        let mut key = {
            let mut hasher = FxHasher::default();
            unread_mentions.hash(&mut hasher);
            unread_events.hash(&mut hasher);
            hasher.finish()
        };
        for (split_id, chat_view) in &self.chat_views {
            let mut hasher = FxHasher::default();
            split_id.hash(&mut hasher);
            chat_view
                .read(cx)
                .canvas_list
                .read(cx)
                .visibility_generation()
                .hash(&mut hasher);
            key ^= hasher.finish();
        }
        if self.activity_read_visibility_key == Some(key) {
            return false;
        }
        self.activity_read_visibility_key = Some(key);

        let mut visible_mentions: Vec<(&str, &str)> = Vec::new();
        let mut live_following_channels = Vec::new();

        for chat_view in self.chat_views.values() {
            let view = chat_view.read(cx);
            let list = view.canvas_list.read(cx);
            if unread_mentions > 0 {
                visible_mentions.extend(
                    list.visible_message_ids()
                        .iter()
                        .map(|message_id| (view.channel.as_str(), message_id.as_ref())),
                );
            }
            if unread_events > 0 && list.is_live_following() {
                live_following_channels.push(view.channel.clone());
            }
        }

        let mentions = backend::mark_visible_global_mentions_read(&visible_mentions);
        let events = backend::mark_recent_twitch_events_read_for_channels(&live_following_channels);
        mentions || events
    }

    /// Get or create a ChatView entity for the given split.
    pub fn get_or_create_chat_view(
        &mut self,
        split_id: SplitId,
        channel: &str,
        container_width: f32,
        cx: &mut Context<Self>,
    ) -> Entity<crate::components::chat::ChatView> {
        let chat_backend = self.app_state.read(cx).chat_backend.clone();
        let chat_state = chat_backend
            .read(cx)
            .get_channel(channel)
            .unwrap_or_else(|| {
                chat_backend.update(cx, |backend, cx| backend.get_or_create_channel(channel, cx))
            });

        if let Some(chat_view) = self.chat_views.get(&split_id)
            && chat_view.read(cx).channel == channel
        {
            return chat_view.clone();
        }

        crate::log_ui!(
            debug,
            split_id = %split_id,
            channel = %channel,
            "Creating new ChatView entity"
        );
        let chat_view = cx.new(|cx| {
            crate::components::chat::ChatView::new(
                channel.to_string(),
                split_id,
                self.app_state.clone(),
                chat_state.clone(),
                container_width,
                cx,
            )
        });
        if self.split_layout.read(cx).is_mod_mode(split_id) {
            chat_view.update(cx, |view, cx| view.set_mod_mode(true, cx));
        }
        if let Some(mode) = self.split_deleted_mode(split_id, cx) {
            chat_view.update(cx, |view, cx| view.set_deleted_mode(Some(mode), cx));
        }
        self.chat_views.insert(split_id, chat_view.clone());
        chat_view
    }

    /// Toggle moderation mode (inline mod buttons) for the active split.
    pub fn toggle_active_split_mod_mode(&mut self, cx: &mut Context<Self>) {
        let Some((_, _, split_id)) = self.active_split_target(cx) else {
            return;
        };
        let enabled = self
            .split_layout
            .update(cx, |layout, _| layout.toggle_mod_mode(split_id));
        if let Some(chat_view) = self.chat_views.get(&split_id).cloned() {
            chat_view.update(cx, |view, cx| view.set_mod_mode(enabled, cx));
        }
        cx.notify();
    }

    /// Scroll the active chat by page (for PageUp/PageDown)
    /// direction: -1 = up, 1 = down
    pub fn scroll_active_chat_by_page(&mut self, direction: i32, cx: &mut Context<Self>) {
        let Some((channel, path, split_id)) = self.active_split_target(cx) else {
            return;
        };

        if let Some(chat_view) = self.chat_views.get(&split_id) {
            chat_view.update(cx, |view, cx| {
                view.canvas_list.update(cx, |list, cx| {
                    list.scroll_by_page(direction);
                    cx.notify();
                });
                view.mark_dirty_for(ChatViewDirtyReason::ManualScroll);
                cx.notify();
            });
        } else {
            crate::log_perf_render!(
                debug,
                channel = %channel,
                path = ?path,
                split_id = %split_id,
                "active scroll by page missed ChatView"
            );
        }
    }

    fn active_chat_scroll_metrics(
        &self,
        cx: &Context<Self>,
    ) -> Option<(f32, f32, f32, f32, usize)> {
        let (_, _, split_id) = self.active_split_target(cx)?;
        let view = self.chat_views.get(&split_id)?.read(cx);
        let list = view.canvas_list.read(cx);
        Some((
            list.scroll_offset(),
            list.scroll_target(),
            list.total_height(),
            list.viewport_height(),
            list.visible_message_ids().len(),
        ))
    }

    /// Scroll the active chat by a wheel-style pixel delta.
    pub fn scroll_active_chat_by_pixels(&mut self, delta_y: f32, cx: &mut Context<Self>) {
        let Some((channel, path, split_id)) = self.active_split_target(cx) else {
            return;
        };

        if let Some(chat_view) = self.chat_views.get(&split_id) {
            chat_view.update(cx, |view, cx| {
                view.canvas_list.update(cx, |list, cx| {
                    list.scroll_by_pixels(delta_y, cx);
                });
                view.mark_dirty_for(ChatViewDirtyReason::ManualScroll);
                cx.notify();
            });
        } else {
            crate::log_perf_render!(
                debug,
                channel = %channel,
                path = ?path,
                split_id = %split_id,
                "active scroll by pixels missed ChatView"
            );
        }
    }

    /// Scroll the active chat to top (for Home key)
    pub fn scroll_active_chat_to_top(&mut self, cx: &mut Context<Self>) {
        let Some((_channel, _path, split_id)) = self.active_split_target(cx) else {
            return;
        };

        if let Some(chat_view) = self.chat_views.get(&split_id) {
            chat_view.update(cx, |view, cx| {
                view.canvas_list.update(cx, |list, cx| {
                    list.scroll_to_top();
                    cx.notify();
                });
                view.mark_dirty_for(ChatViewDirtyReason::ManualScroll);
                cx.notify();
            });
        }
    }

    /// Scroll the active chat to bottom (for End key)
    pub fn scroll_active_chat_to_bottom(&mut self, cx: &mut Context<Self>) {
        let Some((_channel, _path, split_id)) = self.active_split_target(cx) else {
            return;
        };

        if let Some(chat_view) = self.chat_views.get(&split_id) {
            chat_view.update(cx, |view, cx| {
                view.canvas_list.update(cx, |list, cx| {
                    list.scroll_to_bottom();
                    cx.notify();
                });
                view.mark_dirty_for(ChatViewDirtyReason::ManualScroll);
                cx.notify();
            });
        }
    }

    fn render_chat_pane(
        &mut self,
        channel: &str,
        path: Vec<usize>,
        available_width: f32,
        available_height: f32,
        highlights_enabled: bool,
        header_collapsed: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        crate::components::chat::render_chat_pane(
            self,
            channel,
            path,
            available_width,
            available_height,
            highlights_enabled,
            header_collapsed,
            window,
            cx,
        )
    }

    fn split_vertical_chrome(
        &self,
        split_id: Option<SplitId>,
        channel: &str,
        header_height: f32,
        show_input: bool,
        is_active: bool,
        pane_width: f32,
        cx: &App,
    ) -> SplitVerticalChrome {
        let pane = split_id.and_then(|split_id| self.split_panes.get(&split_id));
        let input = if show_input {
            split_id.map_or(config::CHAT_INPUT_HEIGHT_PX, |split_id| {
                let reply_visible = pane.is_some_and(|pane| pane.read(cx).reply_to.is_some());
                self.chat_input_renderer
                    .read(cx)
                    .input_surface_metrics_for_split(split_id, reply_visible)
                    .total_height
            })
        } else {
            0.0
        };
        let search = if is_active
            && self
                .overlay_manager
                .read(cx)
                .search_modal
                .read(cx)
                .is_docked_open()
        {
            crate::components::overlays::DOCKED_SEARCH_HEIGHT
        } else {
            0.0
        };
        let concrete_channel = backend::browser_watching::resolve_layout_channel(channel);
        let channel = concrete_channel.as_deref().unwrap_or(channel);
        let pinned = if header_height == 0.0
            || pane_width <= 30.0
            || pane.is_none()
            || backend::whispers::is_whispers_channel(channel)
        {
            0.0
        } else if let Some(pinned) = backend::pinned_chat::get_cached(channel) {
            let dismissed = pane.is_some_and(|pane| {
                pane.read(cx).is_pinned_chat_dismissed(
                    &pinned.message_id,
                    pinned.updated_at.as_deref(),
                    &pinned.text,
                )
            });
            if dismissed {
                0.0
            } else {
                crate::components::chat::chat_pane::PINNED_CHAT_COLLAPSED_STRIP_HEIGHT_PX
            }
        } else {
            0.0
        };
        SplitVerticalChrome {
            header: header_height,
            input,
            search,
            pinned,
        }
    }

    fn render_cached_split_leaf(
        &mut self,
        split_id: SplitId,
        channel: &str,
        path: Vec<usize>,
        available_width: f32,
        available_height: f32,
        highlights_enabled: bool,
        header_collapsed: bool,
        overlay: Option<AnyElement>,
        _window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let pane_width = available_width.max(0.0);
        let pane_height = available_height.max(0.0);
        let path_key = SplitLayoutManager::path_key(&path);
        let path_key_for_canvas = path_key.clone();
        let bounds_entity = cx.entity().clone();
        let drop_channel = channel.to_string();

        let (show_split_input, is_active, drag_mode) = {
            let split_layout = self.split_layout.read(cx);
            (
                !split_layout.is_unified_input_mode(),
                split_layout.get_active_split_path() == path,
                split_layout.is_drag_mode_active(),
            )
        };

        let (
            input_focus,
            reply_state_hash,
            pinned_chat_dismissal_version,
            pinned_chat_expansion_version,
            split_pane_id,
        ) = match self.split_panes.get(&split_id) {
            Some(pane) => {
                let entity_id = pane.entity_id();
                let pane_data = pane.read(cx);
                (
                    pane_data.focus_handle.clone(),
                    reply_state_gate_hash(pane_data.reply_to.as_ref()),
                    pane_data.pinned_chat_dismissal_version(),
                    pane_data.pinned_chat_expansion_version(),
                    Some(entity_id),
                )
            }
            None => (
                self.input_focus.clone(),
                reply_state_gate_hash(None),
                0,
                0,
                None,
            ),
        };
        let click_focus = if show_split_input {
            input_focus.clone()
        } else {
            self.input_focus.clone()
        };

        let compact_split_header = backend::settings::get_compact_split_header();
        let header_height = if header_collapsed {
            0.0
        } else {
            main_split_header_height(compact_split_header)
        };
        let chrome = self.split_vertical_chrome(
            Some(split_id),
            channel,
            header_height,
            show_split_input,
            is_active,
            pane_width,
            cx,
        );
        let input_height = chrome.input;
        let search_height = chrome.search;
        let pinned_chat_strip_height = chrome.pinned;
        let message_list_height = (pane_height - chrome.height()).max(0.0);
        let (stream_is_live, stream_info_hash, uptime_minutes) = {
            let app_state = self.app_state.read(cx);
            let info = app_state.stream_info.get(channel).and_then(|i| i.as_ref());
            (
                info.is_some_and(|info| info.is_live),
                stream_info_gate_hash(info),
                crate::backend::live_uptime_minutes(info),
            )
        };
        let show_timestamp = backend::settings::should_show_message_timestamps(stream_is_live);

        let chat_view = if pane_width > 1.0 && message_list_height > 1.0 {
            let chat_view = self.get_or_create_chat_view(split_id, channel, pane_width, cx);
            chat_view.update(cx, |view, cx| {
                view.update_render_params(
                    pane_width,
                    message_list_height,
                    highlights_enabled,
                    show_timestamp,
                    is_active,
                    cx,
                );
            });
            Some(chat_view)
        } else {
            None
        };

        let split_settings_open = self
            .overlay_manager
            .read(cx)
            .is_split_settings_open(path_key);
        let layout_locked = self.app_state.read(cx).active_tab().is_locked;
        let mod_mode = self.split_layout.read(cx).is_mod_mode(split_id);

        let gate = SplitLeafParamsGate {
            pane_width,
            pane_height,
            input_height,
            search_height,
            highlights_enabled,
            show_split_input,
            drag_mode,
            is_active_split: is_active,
            split_settings_open,
            compact_split_header,
            header_collapsed,
            layout_locked,
            mod_mode,
            room_state_version: crate::backend::irc::room_state::room_state_version(),
            stream_info_hash,
            uptime_minutes,
            pinned_chat_version: crate::backend::pinned_chat::version(),
            pinned_chat_dismissal_version,
            pinned_chat_expansion_version,
            streamer_mode_active: crate::backend::streamer_mode::is_streamer_mode_active(),
            reply_state_hash,
            split_pane_id,
        };

        let retained = self
            .split_leaf_views
            .get(&split_id)
            .filter(|view| view.read(cx).gate_matches(&gate, channel, &path))
            .cloned();

        let split_leaf_view = match retained {
            Some(view) => view,
            None => {
                let stream_info = self
                    .app_state
                    .read(cx)
                    .stream_info
                    .get(channel)
                    .cloned()
                    .flatten();
                let room_state = crate::backend::irc::room_state::get_room_state(channel);
                let reply_state = self
                    .split_panes
                    .get(&split_id)
                    .and_then(|pane| pane.read(cx).reply_to.clone());
                let params = SplitLeafRenderParams {
                    split_id,
                    channel: channel.to_string(),
                    path: path.clone(),
                    pane_width,
                    pane_height,
                    input_height,
                    search_height,
                    highlights_enabled,
                    show_split_input,
                    drag_mode,
                    is_active_split: is_active,
                    stream_info,
                    room_state,
                    split_settings_open,
                    compact_split_header,
                    header_collapsed,
                    layout_locked,
                    mod_mode,
                    input_focus: input_focus.clone(),
                    reply_state,
                };
                self.get_or_create_split_leaf_view(split_id, gate, params, cx)
            }
        };

        let click_path = path;

        div()
            .id(("split-leaf", split_id.as_u64()))
            .w(px(pane_width))
            .h(px(pane_height))
            .min_h_0()
            .min_w_0()
            .flex_shrink_0()
            .overflow_hidden()
            .relative()
            .bg(colors::bg_void())
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _event: &MouseDownEvent, window, cx| {
                    this.set_active_split(click_path.clone(), cx);
                    window.focus(&click_focus, cx);
                }),
            )
            .children(chat_view.map(|chat_view| {
                div()
                    .absolute()
                    .left_0()
                    .top(px(header_height + search_height + pinned_chat_strip_height))
                    .w(px(pane_width))
                    .h(px(message_list_height))
                    .child(
                        AnyView::from(chat_view)
                            .cached(StyleRefinement::default().flex().flex_row().size_full()),
                    )
            }))
            .child(
                AnyView::from(split_leaf_view)
                    .cached(StyleRefinement::default().flex().flex_col().size_full()),
            )
            .child(
                div()
                    .id(("file-drop-highlight", split_id.as_u64()))
                    .absolute()
                    .inset_0()
                    .drag_over::<ExternalPaths>(|style, _, _, _| {
                        if backend::image_uploader::ImageFileSource::Dropped
                            .upload_enabled(&backend::settings::get_image_uploader_config())
                        {
                            style.bg(colors::accent_transparent(0.08))
                        } else {
                            style
                        }
                    }),
            )
            .children(overlay)
            .child(
                canvas(
                    move |bounds, _window, cx| {
                        bounds_entity.update(cx, |app, cx| {
                            app.file_drop_targets
                                .insert(split_id, (drop_channel.clone(), bounds));
                            let hover_changed = app.split_layout.update(cx, |layout, _| {
                                layout.register_leaf_bounds(
                                    Some(split_id),
                                    path_key_for_canvas.clone(),
                                    bounds,
                                );
                                layout.recompute_hovered_split()
                                    && layout.is_add_split_mode_active()
                            });
                            if hover_changed {
                                cx.notify();
                            }
                        });
                    },
                    |_bounds, _: (), _window, _cx| {},
                )
                .absolute()
                .inset_0(),
            )
            .into_any_element()
    }

    pub fn open_settings(&self, selected: Option<SettingsSectionId>, cx: &mut Context<Self>) {
        self.overlay_manager.update(cx, |overlays, cx| {
            overlays.open_settings(selected, cx);
        });
        cx.notify();
    }

    fn open_mod_log(&self, cx: &mut Context<Self>) {
        self.overlay_manager.update(cx, |overlays, cx| {
            overlays.open_mod_log(cx);
        });
        cx.notify();
    }

    /// Execute a hotkey action (delegates to event_handlers::dispatch_hotkey_action)
    pub fn execute_hotkey_action(
        &mut self,
        action: backend::hotkeys::HotkeyAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        crate::event_handlers::dispatch_hotkey_action(self, action, window, cx);
    }

    fn render_user_card(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let user_card = self.overlay_manager.read(cx).user_card.clone();
        let is_open = user_card.read(cx).is_open();
        if !is_open {
            return div().into_any_element();
        }

        user_card.into_any_element()
    }

    fn render_chatter_list(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let chatter_list = self.overlay_manager.read(cx).chatter_list.clone();
        let is_open = chatter_list.read(cx).state.is_open;
        if !is_open {
            return div().into_any_element();
        }

        chatter_list.into_any_element()
    }

    fn render_confirmation_modal(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let modal = self.overlay_manager.read(cx).confirmation_modal.clone();
        let is_open = modal.read(cx).state.is_open;
        if !is_open {
            return div().into_any_element();
        }

        modal.into_any_element()
    }

    fn render_toast_manager(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let toast_mgr = self.overlay_manager.read(cx).toast_manager.clone();
        let has_toasts = toast_mgr.read(cx).has_toasts();
        if !has_toasts {
            return div().into_any_element();
        }

        toast_mgr.into_any_element()
    }

    fn render_metrics_overlay(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let metrics = self.overlay_manager.read(cx).metrics_overlay.clone();
        metrics.into_any_element()
    }

    fn render_thread_viewer(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let thread_viewer = self.overlay_manager.read(cx).thread_viewer.clone();
        let is_open = thread_viewer.read(cx).is_open();
        if !is_open {
            return div().into_any_element();
        }

        crate::components::overlays::clear_global_tooltip();
        thread_viewer.into_any_element()
    }

    fn render_mod_log_modal(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let mod_log = self.overlay_manager.read(cx).mod_log_viewer.clone();
        let is_open = mod_log.read(cx).state.is_open;
        if !is_open {
            return div().into_any_element();
        }

        crate::components::overlays::clear_global_tooltip();
        AnyView::from(mod_log).into_any_element()
    }

    fn render_search_modal(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let search = self.overlay_manager.read(cx).search_modal.clone();
        let is_open = search.read(cx).is_expanded();
        if !is_open {
            return div().into_any_element();
        }

        AnyView::from(search).into_any_element()
    }

    fn render_emote_picker(
        &mut self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let emote_picker = self.overlay_manager.read(cx).emote_picker.clone();

        let is_open = emote_picker.read(cx).state.is_open;
        if !is_open {
            return div().into_any_element();
        }

        let split_id = emote_picker.read(cx).state.split_id;
        if self
            .app_state
            .read(cx)
            .active_tab()
            .split_path_for_id(split_id)
            .is_none()
        {
            emote_picker.update(cx, |picker, cx| picker.close(false, cx));
            return div().into_any_element();
        }
        let bounds = self.emote_picker_composer_bounds(split_id, window.viewport_size(), cx);
        emote_picker.update(cx, |picker, cx| picker.set_composer_bounds(bounds, cx));
        let app = cx.entity().clone();
        let picker_for_anchor = emote_picker.clone();
        div()
            .id("emote-picker-anchor")
            .absolute()
            .inset_0()
            .child(
                canvas(
                    move |_, window, cx| {
                        // Leaf bounds are published during prepaint, after the app's render.
                        app.update(cx, |app, cx| {
                            let bounds = app.emote_picker_composer_bounds(
                                split_id,
                                window.viewport_size(),
                                cx,
                            );
                            picker_for_anchor
                                .update(cx, |picker, cx| picker.set_composer_bounds(bounds, cx));
                        });
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .inset_0(),
            )
            .child(emote_picker)
            .into_any_element()
    }

    /// Render the context menu overlay
    fn render_context_menu(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let context_menu = self.overlay_manager.read(cx).context_menu.clone();
        let is_open = context_menu.read(cx).state.is_open;

        if !is_open {
            return div().into_any_element();
        }

        modal_backdrop_style()
            .id("context-menu-backdrop")
            .on_mouse_move(|_, _, cx| cx.stop_propagation())
            .on_mouse_up(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_mouse_up(MouseButton::Right, |_, _, cx| cx.stop_propagation())
            .on_mouse_up(MouseButton::Middle, |_, _, cx| cx.stop_propagation())
            .on_scroll(|_, _, cx| cx.stop_propagation())
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    this.overlay_manager
                        .update(cx, |o, cx| o.close_context_menu(cx));
                    cx.stop_propagation();
                }),
            )
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(|this, _, _, cx| {
                    this.overlay_manager
                        .update(cx, |o, cx| o.close_context_menu(cx));
                    cx.stop_propagation();
                }),
            )
            .on_mouse_down(MouseButton::Middle, |_, _, cx| cx.stop_propagation())
            .child(context_menu)
            .into_any_element()
    }

    /// Close the split that currently has focus (Ctrl+W).
    ///
    /// Closing the last split leaves the tab open on an empty channel prompt.
    pub fn close_active_split(&mut self, cx: &mut Context<Self>) {
        let path = self.split_layout.read(cx).get_active_split_path().to_vec();
        self.close_pane(path, cx);
    }

    pub fn close_pane(&mut self, path: Vec<usize>, cx: &mut Context<Self>) {
        if self.refuse_locked_split_layout(cx) {
            return;
        }
        // Drags hold positional paths; a structural change must not retarget them.
        self.cancel_pointer_drags(cx);
        let key = SplitLayoutManager::path_key(&path);
        self.split_layout
            .update(cx, |layout, _| layout.forget_split_bounds(&key));

        let (split_id, channel, active_tab_idx, restore_anchor) = {
            let app_state = self.app_state.read(cx);
            let active_tab = app_state.active_tab();
            (
                active_tab.layout.get_split_id_at_path(&path),
                active_tab.layout.get_channel_at_path(&path),
                app_state.active_tab_idx,
                active_tab
                    .reopen_anchor_for_path(&path)
                    .map(|(split_id, direction)| ClosedSplitRestoreAnchor {
                        split_id,
                        direction,
                    }),
            )
        };
        if let Some(split_id) = split_id {
            self.chat_input_renderer.update(cx, |renderer, _cx| {
                renderer.input_manager.placeholder_inputs.remove(&split_id);
                renderer.input_manager.clear_draft_for_split(split_id);
                if renderer.input_manager.focused_placeholder == Some(split_id) {
                    renderer.input_manager.focused_placeholder = None;
                }
            });
            self.chat_views.remove(&split_id);
        }
        let pending_previous = split_id.and_then(|split_id| {
            self.app_state.update(cx, |state, _| {
                state.pending_channel_changes.remove(&split_id)
            })
        });
        let closed_channel = channel
            .filter(|channel| !is_placeholder(channel))
            .or(pending_previous);
        if let Some(channel) = closed_channel.as_ref() {
            self.last_closed_split = Some(ClosedSplitHotkeyState {
                channel: channel.clone(),
                tab_index: active_tab_idx,
                restore_anchor,
            });
        }

        self.app_state.update(cx, |state, cx| {
            let workspace = state.active_tab_mut();
            workspace.close_pane_by_path(&path);
            cx.notify();
        });
        if let Some(channel) = closed_channel {
            self.cleanup_channel_caches(&channel, cx);
        }
        self.save_workspace_layout(cx);
        self.refocus_active_input(cx);
    }

    pub fn reopen_last_closed_split(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(closed_split) = self.last_closed_split.clone() else {
            return false;
        };
        self.cancel_pointer_drags(cx);

        if self
            .app_state
            .read(cx)
            .tabs
            .get(closed_split.tab_index)
            .is_some()
        {
            self.app_state.update(cx, |state, cx| {
                state.switch_tab(closed_split.tab_index, cx);
            });
        }

        let reopened_path = closed_split.restore_anchor.and_then(|anchor| {
            self.create_placeholder_split_adjacent_to_split(anchor.split_id, anchor.direction, cx)
        });

        let Some(path) = reopened_path.or_else(|| {
            self.create_placeholder_split_adjacent_to_active(AddSplitDirection::Right, cx)
        }) else {
            return false;
        };

        self.set_channel_for_path(path, closed_split.channel, cx);
        self.last_closed_split = None;
        true
    }

    pub fn set_channel_for_path(
        &mut self,
        path: Vec<usize>,
        channel: String,
        cx: &mut Context<Self>,
    ) {
        let Ok(trimmed) = crate::utils::channel_input::normalize_channel_input(&channel) else {
            crate::log_ui!(debug, "Ignoring invalid channel name {:?}", channel);
            return;
        };
        let split_id = self
            .app_state
            .read(cx)
            .active_tab()
            .layout
            .get_split_id_at_path(&path);
        let pending_previous = split_id.and_then(|split_id| {
            self.app_state
                .read(cx)
                .pending_channel_changes
                .get(&split_id)
                .cloned()
        });
        if let (Some(split_id), Some(previous)) = (split_id, pending_previous.as_ref())
            && (trimmed.is_empty()
                || previous.eq_ignore_ascii_case(&trimmed)
                || crate::backend::browser_watching::resolve_layout_channel(previous)
                    .is_some_and(|resolved| resolved.eq_ignore_ascii_case(&trimmed)))
        {
            self.restore_pending_channel_change(split_id, cx);
            return;
        }
        if trimmed.is_empty() {
            return;
        }
        let is_watching = crate::backend::browser_watching::is_watching_channel(&trimmed);

        let changed = self.app_state.update(cx, |state, cx| {
            let tab = state.active_tab_mut();
            if tab.set_channel_at_path(&path, trimmed.clone()) {
                let concrete_channel = if is_watching {
                    crate::backend::browser_watching::current_channel()
                } else {
                    Some(trimmed.clone())
                };
                if let Some(concrete_channel) = concrete_channel
                    && !crate::benchmark_ui_fixture::disposable_workspace_enabled()
                {
                    if !backend::whispers::is_whispers_channel(&concrete_channel) {
                        backend::preload_channel_emotes_predictive(&concrete_channel);
                    }

                    backend::connect_channel(&concrete_channel);
                }

                cx.notify();
                true
            } else {
                false
            }
        });

        if let Some(split_id) = split_id {
            self.chat_input_renderer.update(cx, |renderer, _cx| {
                renderer.input_manager.placeholder_inputs.remove(&split_id);
                if renderer.input_manager.focused_placeholder == Some(split_id) {
                    renderer.input_manager.focused_placeholder = None;
                }
                if changed {
                    renderer.input_manager.reset_history_navigation(split_id);
                }
            });
        }

        if changed {
            if let Some(split_id) = split_id
                && let Some(previous) = self.app_state.update(cx, |state, _| {
                    state.pending_channel_changes.remove(&split_id)
                })
            {
                let previous = crate::backend::browser_watching::resolve_layout_channel(&previous)
                    .unwrap_or(previous);
                self.cleanup_channel_caches(&previous, cx);
            }
            self.save_workspace_layout(cx);
            if self.split_layout.read(cx).get_active_split_path() == path.as_slice()
                && let Some(split_id) = split_id
            {
                self.refocus_active_input(cx);
                self.notify_split_leaf(split_id, cx);
            }
        }
    }

    /// Persist the channel-header visibility for exactly one active-tab leaf.
    /// The state stays on the leaf, so its stable identity carries it through
    /// channel replacement and split move/reorder operations.
    pub fn set_split_header_collapsed(
        &mut self,
        path: Vec<usize>,
        collapsed: bool,
        cx: &mut Context<Self>,
    ) {
        let changed = self.app_state.update(cx, |state, _| {
            std::sync::Arc::make_mut(&mut state.active_tab_mut().layout)
                .set_header_collapsed_at_path(&path, collapsed)
        });
        if changed {
            self.save_workspace_layout(cx);
            cx.notify();
        }
    }

    /// The deleted-message override saved on a split's leaf, if any.
    fn split_deleted_mode(
        &self,
        split_id: SplitId,
        cx: &App,
    ) -> Option<backend::settings::DeletedMessageMode> {
        self.app_state.read(cx).tabs.iter().find_map(|tab| {
            let path = tab.split_path_for_id(split_id)?;
            tab.layout.deleted_mode_at_path(&path)
        })
    }

    /// Choose how one active-tab split presents moderator-removed messages;
    /// `None` follows the global setting. Saved on the leaf like the header state.
    pub fn set_split_deleted_mode(
        &mut self,
        path: Vec<usize>,
        mode: Option<backend::settings::DeletedMessageMode>,
        cx: &mut Context<Self>,
    ) {
        let (changed, split_id) = self.app_state.update(cx, |state, _| {
            let tab = state.active_tab_mut();
            let split_id = tab.layout.get_split_id_at_path(&path);
            let changed =
                std::sync::Arc::make_mut(&mut tab.layout).set_deleted_mode_at_path(&path, mode);
            (changed, split_id)
        });
        if !changed {
            return;
        }
        if let Some(chat_view) = split_id.and_then(|id| self.chat_views.get(&id).cloned()) {
            chat_view.update(cx, |view, cx| view.set_deleted_mode(mode, cx));
        }
        self.save_workspace_layout(cx);
        cx.notify();
    }

    /// Save all tabs to disk.
    /// Called automatically after any tab or layout modification
    fn save_workspace_layout(&self, cx: &mut Context<Self>) {
        self.save_tabs(cx);
    }

    /// Start dragging a split divider for resizing
    fn start_split_drag(
        &mut self,
        path: Vec<usize>,
        child_idx: usize,
        orientation: SplitOrientation,
        pos: Point<Pixels>,
        container_size: f32,
        cx: &mut Context<Self>,
    ) {
        let tab_id = self.app_state.read(cx).active_tab().id.clone();
        self.split_layout.update(cx, |layout, _| {
            layout.start_resize_drag(SplitDragState {
                tab_id,
                path,
                child_idx,
                orientation,
                start_pos: pos,
                container_size,
            });
        });
        // Repaint so the window-level drag tracker is registered for the next move.
        cx.notify();
    }

    /// Press on one band of a 2x2 block's shared divider.
    ///
    /// The shared divider spans the whole block, so dragging it resizes the far
    /// band too — the "resize the top row, the bottom row moves" defect. Rotate
    /// the block first so the pressed band owns its own divider, then start a
    /// perfectly ordinary drag on that divider. The rotation only runs while
    /// both band dividers agree, so nothing visibly moves at the moment of the
    /// press, and every pane keeps its `SplitId`.
    fn start_grid_divider_drag(
        &mut self,
        block_path: Vec<usize>,
        band_idx: usize,
        orientation: SplitOrientation,
        pos: Point<Pixels>,
        container_size: f32,
        cx: &mut Context<Self>,
    ) {
        let displayed_grid = self
            .app_state
            .read(cx)
            .active_tab()
            .layout
            .get_node_at_path(&block_path)
            .filter(|node| node.grid_band_split_ratio().is_some())
            .cloned()
            .and_then(|mut grid| {
                let layout = self.split_layout.read(cx);
                let displayed_ratios = |node: &SplitNode, path: &[usize]| {
                    let SplitNode::Split {
                        orientation,
                        children,
                        ..
                    } = node
                    else {
                        return None;
                    };
                    let (width, height) = layout
                        .split_bounds
                        .get(&SplitLayoutManager::path_key(path))?;
                    let extent = match orientation {
                        SplitOrientation::Vertical => *width,
                        SplitOrientation::Horizontal => *height,
                    } - children.len().saturating_sub(1) as f32
                        * crate::managers::HANDLE_VISUAL_SIZE;
                    let minimums = layout.child_minimum_extents(path, children.len(), orientation);
                    Some(node.ratios_with_minimums(extent.max(0.0), &minimums))
                };
                let root_ratios = displayed_ratios(&grid, &block_path)?;
                if let SplitNode::Split {
                    ratios, children, ..
                } = &mut grid
                {
                    *ratios = root_ratios;
                    for (idx, child) in children.iter_mut().enumerate() {
                        let mut path = block_path.clone();
                        path.push(idx);
                        let child_ratios = displayed_ratios(child, &path)?;
                        if let SplitNode::Split { ratios, .. } = child {
                            *ratios = child_ratios;
                        }
                    }
                }
                grid.grid_band_split_ratio()?;
                Some(grid)
            });
        let rotated = self.app_state.update(cx, |state, _cx| {
            let tab = state.active_tab_mut();
            let Some(grid) = displayed_grid else {
                return false;
            };
            if tab.is_locked {
                return false;
            }
            // Commit the displayed ratios before rotating so pixel clamps cannot
            // make a band drag jump at the moment it changes tree orientation.
            std::sync::Arc::make_mut(&mut tab.layout).set_node_at_path(&block_path, grid);
            tab.transpose_grid_block(&block_path)
        });

        if !rotated {
            self.start_split_drag(block_path, 0, orientation, pos, container_size, cx);
            return;
        }

        self.split_layout.update(cx, |layout, _| {
            layout.remap_after_grid_transpose(&block_path);
        });

        let mut drag_path = block_path;
        drag_path.push(band_idx);
        self.start_split_drag(drag_path, 0, orientation, pos, container_size, cx);
        self.save_workspace_layout(cx);
        cx.notify();
    }

    /// Update the split ratio during drag
    pub fn update_split_drag(&mut self, pos: Point<Pixels>, cx: &mut Context<Self>) {
        self.cancel_stale_pointer_drags(cx);
        let update_info = {
            let layout = self.split_layout.read(cx);
            let Some(drag) = layout.resize_drag() else {
                return;
            };

            let delta_pixels: f32 = match drag.orientation {
                SplitOrientation::Vertical => (pos.x - drag.start_pos.x).into(),
                SplitOrientation::Horizontal => (pos.y - drag.start_pos.y).into(),
            };

            if drag.container_size <= 0.0 {
                return;
            }
            (
                drag.path.clone(),
                drag.child_idx,
                delta_pixels,
                drag.orientation.clone(),
                drag.container_size,
            )
        };

        let (path, child_idx, delta_pixels, orientation, previous_extent) = update_info;
        let child_count = match self
            .app_state
            .read(cx)
            .active_tab()
            .layout
            .get_node_at_path(&path)
        {
            Some(SplitNode::Split { children, .. }) => children.len(),
            _ => return,
        };
        let extent = previous_extent;
        let minimums =
            self.split_layout
                .read(cx)
                .child_minimum_extents(&path, child_count, &orientation);

        let applied_pixels = self.app_state.update(cx, |state, _cx| {
            let tab = state.active_tab_mut();
            if tab.is_locked {
                return 0.0;
            }
            std::sync::Arc::make_mut(&mut tab.layout).adjust_ratio_with_minimums_applied(
                &path,
                child_idx,
                delta_pixels / extent,
                extent,
                &minimums,
            ) * extent
        });

        self.split_layout.update(cx, |layout, _| {
            if let Some(drag) = &mut layout.resize_drag {
                match drag.orientation {
                    SplitOrientation::Vertical => drag.start_pos.x += px(applied_pixels),
                    SplitOrientation::Horizontal => drag.start_pos.y += px(applied_pixels),
                }
            }
        });

        if applied_pixels != 0.0 {
            cx.notify();
        }
    }

    /// End the split drag and persist the layout
    pub fn end_split_drag(&mut self, cx: &mut Context<Self>) {
        let had_drag = self
            .split_layout
            .update(cx, |layout, _| layout.resize_drag.take().is_some());
        if had_drag {
            self.save_workspace_layout(cx);
            cx.notify();
        }
    }

    /// Release an active resize from the divider's capture-safe pointer owner.
    /// Multiple divider `mouse_up_out` listeners can observe the same release;
    /// the active-state check keeps the operation and persistence single-shot.
    fn end_split_drag_from_pointer(&mut self, cx: &mut Context<Self>) {
        if !self.split_layout.read(cx).is_resizing() {
            return;
        }
        crate::managers::frame_rate_controller::record_interaction();
        self.end_split_drag(cx);
    }

    pub(crate) fn activate_tab_split(
        &mut self,
        tab_index: usize,
        path: Vec<usize>,
        cx: &mut Context<Self>,
    ) {
        if self.app_state.read(cx).active_tab_idx != tab_index {
            self.app_state
                .update(cx, |state, cx| state.switch_tab(tab_index, cx));
            self.sync_active_split_from_active_tab(cx);
            self.refocus_active_input(cx);
        }
        self.set_active_split(path, cx);
    }

    /// Set the active split pane
    pub fn set_active_split(&mut self, path: Vec<usize>, cx: &mut Context<Self>) {
        let current_path = self.split_layout.read(cx).get_active_split_path().to_vec();
        if current_path != path {
            let target_split_id = self
                .app_state
                .read(cx)
                .active_tab()
                .layout
                .get_split_id_at_path(&path);

            self.overlay_manager
                .read(cx)
                .emote_picker
                .clone()
                .update(cx, |picker, cx| {
                    if picker.state.is_open && Some(picker.state.split_id) != target_split_id {
                        picker.close(false, cx);
                    }
                });

            self.autocomplete(cx).update(cx, |ac, _cx| {
                if ac.state.is_open && Some(ac.state.split_id) != target_split_id {
                    ac.state.close();
                }
            });

            self.update_active_chat_view_flags(&path, cx);

            self.split_layout
                .update(cx, |layout, _| layout.set_active_split(path));
            let persisted_path = self.split_layout.read(cx).get_active_split_path().to_vec();
            let changed = self.app_state.update(cx, |state, _| {
                state.active_tab_mut().set_active_split_path(persisted_path)
            });
            if changed {
                self.save_tabs(cx);
            }
            self.refocus_active_input(cx);
            cx.notify();
        }
    }

    fn get_or_create_split_leaf_view(
        &mut self,
        split_id: SplitId,
        gate: SplitLeafParamsGate,
        params: SplitLeafRenderParams,
        cx: &mut Context<Self>,
    ) -> Entity<SplitLeafView> {
        if let Some(view) = self.split_leaf_views.get(&split_id) {
            let view = view.clone();
            view.update(cx, |view, cx| view.update_params(gate, params, cx));
            return view;
        }

        let handles = SplitLeafViewHandles {
            app: cx.entity().downgrade(),
            chat_input_renderer: self.chat_input_renderer.clone(),
            chat_header_renderer: self.chat_header_renderer.clone(),
            overlay_manager: self.overlay_manager.clone(),
            chat_backend: self.app_state.read(cx).chat_backend.clone(),
        };
        let view = cx.new(|cx| SplitLeafView::new(handles, gate, params, cx));
        self.split_leaf_views.insert(split_id, view.clone());
        view
    }

    /// Get or create a SplitPane entity for a given SplitId and channel
    pub fn get_or_create_split_pane(
        &mut self,
        split_id: crate::state::SplitId,
        channel: String,
        cx: &mut Context<Self>,
    ) -> Entity<crate::state::SplitPane> {
        if let Some(pane) = self.split_panes.get(&split_id) {
            let pane_channel = pane.read(cx).channel.clone();
            if pane_channel != channel {
                pane.update(cx, |p, cx| {
                    p.set_channel(channel, cx);
                });
            }
            pane.clone()
        } else {
            let pane = cx.new(|cx| crate::state::SplitPane::with_id(split_id, channel, cx));

            let pane_clone = pane.clone();
            let subscription = cx.subscribe(&pane, move |app, _entity, event, cx| {
                app.handle_split_pane_event(pane_clone.clone(), event, cx);
            });

            self.split_panes.insert(split_id, pane.clone());
            self.split_pane_subscriptions.insert(split_id, subscription);
            pane
        }
    }

    /// Handle events emitted by SplitPane entities
    fn handle_split_pane_event(
        &mut self,
        pane: Entity<crate::state::SplitPane>,
        event: &crate::state::SplitPaneEvent,
        cx: &mut Context<Self>,
    ) {
        use crate::state::SplitPaneEvent;

        let (split_id, _channel) = {
            let pane_data = pane.read(cx);
            (pane_data.id, pane_data.channel.clone())
        };

        match event {
            SplitPaneEvent::ChannelChanged { new_channel } => {
                crate::log_ui!(
                    debug,
                    "SplitPane {} changed to channel: {}",
                    split_id,
                    new_channel
                );
            }
        }
    }

    /// Ensure all splits in the current layout have corresponding SplitPane entities
    /// and clean up any orphaned panes that are no longer in the layout.
    /// This should be called when the layout changes.
    pub fn ensure_split_panes_for_layout(&mut self, cx: &mut Context<Self>) -> bool {
        let reconcile_key = self.split_reconcile_key(cx);
        if self.last_split_reconcile_key == Some(reconcile_key) {
            return false;
        }

        let panes_before = self.split_panes.len();
        let leaves_before = self.split_leaf_views.len();
        let mut changed = false;

        if layout_has_missing_split_ids(self.app_state.read(cx).active_tab().layout.as_ref()) {
            let mut layout = (*self.app_state.read(cx).active_tab().layout).clone();
            if layout.ensure_split_ids() {
                self.app_state.update(cx, |state, _| {
                    let tab = state.active_tab_mut();
                    tab.layout = std::sync::Arc::new(layout);
                });
                changed = true;
            }
        }

        let current_layout = self.app_state.read(cx).active_tab().layout.clone();

        fn collect_split_info(node: &SplitNode, result: &mut Vec<(crate::state::SplitId, String)>) {
            match node {
                SplitNode::Leaf {
                    split_id, channel, ..
                } => {
                    if let Some(id) = split_id {
                        result.push((*id, channel.clone()));
                    }
                }
                SplitNode::Split { children, .. } => {
                    for child in children {
                        collect_split_info(child, result);
                    }
                }
            }
        }

        let mut split_info = Vec::new();
        collect_split_info(&current_layout, &mut split_info);

        for (split_id, channel) in split_info {
            self.get_or_create_split_pane(split_id, channel, cx);
        }

        self.cleanup_orphaned_split_panes(cx);

        self.last_split_reconcile_key = Some(self.split_reconcile_key(cx));

        changed
            || panes_before != self.split_panes.len()
            || leaves_before != self.split_leaf_views.len()
    }

    /// Content hash of everything [`Self::ensure_split_panes_for_layout`] and
    /// [`Self::cleanup_orphaned_split_panes`] consume: the active tab plus every
    /// tab's split ids, channels and tree shape. Allocation-free.
    fn split_reconcile_key(&self, cx: &Context<Self>) -> u64 {
        use std::hash::{Hash, Hasher};

        fn hash_node(node: &SplitNode, hasher: &mut impl Hasher) {
            match node {
                SplitNode::Leaf {
                    split_id,
                    channel,
                    header_collapsed,
                    ..
                } => {
                    0u8.hash(hasher);
                    split_id.hash(hasher);
                    channel.hash(hasher);
                    header_collapsed.hash(hasher);
                }
                SplitNode::Split {
                    split_id, children, ..
                } => {
                    1u8.hash(hasher);
                    split_id.hash(hasher);
                    children.len().hash(hasher);
                    for child in children {
                        hash_node(child, hasher);
                    }
                }
            }
        }

        let app_state = self.app_state.read(cx);
        let mut hasher = ahash::AHasher::default();
        app_state.active_tab_idx.hash(&mut hasher);
        app_state.tabs.len().hash(&mut hasher);
        for tab in app_state.tabs.iter() {
            tab.id.hash(&mut hasher);
            hash_node(&tab.layout, &mut hasher);
        }
        hasher.finish()
    }

    /// Remove SplitPane entities that are no longer in the layout
    pub fn cleanup_orphaned_split_panes(&mut self, cx: &mut Context<Self>) {
        let active_ids: std::collections::HashSet<_> = self
            .app_state
            .read(cx)
            .tabs
            .iter()
            .flat_map(|tab| tab.layout.collect_split_ids())
            .collect();

        let orphaned_ids: Vec<_> = self
            .split_panes
            .keys()
            .filter(|id| !active_ids.contains(id))
            .copied()
            .collect();

        for id in orphaned_ids {
            self.split_pane_subscriptions.remove(&id);
            self.split_panes.remove(&id);
        }

        self.split_leaf_views
            .retain(|id, _| active_ids.contains(id));
        self.chat_views.retain(|id, _| active_ids.contains(id));
    }

    /// Copy selected text from the active chat to clipboard.
    ///
    /// Returns whether the active chat owned a selection and copied it. The
    /// root key router uses this to give a highlighted chat range precedence
    /// over the still-focused composer without breaking composer Ctrl+C when
    /// no chat text is selected.
    pub fn copy_selection(&mut self, cx: &mut Context<Self>) -> bool {
        let Some((_channel, _path, split_id)) = self.active_split_target(cx) else {
            return false;
        };

        self.chat_views.get(&split_id).is_some_and(|chat_view| {
            chat_view.update(cx, |view, cx| {
                view.canvas_list
                    .update(cx, |list, _cx| list.copy_selection_to_clipboard())
            })
        })
    }

    /// Start dragging a split pane
    pub fn start_pane_drag(
        &mut self,
        path: Vec<usize>,
        channel: String,
        ctrl_initiated: bool,
        cx: &mut Context<Self>,
    ) {
        if self.app_state.read(cx).active_tab().is_locked {
            return;
        }
        let tab_id = self.app_state.read(cx).active_tab().id.clone();
        self.split_layout.update(cx, |layout, _| {
            layout.start_pane_drag(tab_id, path, channel, ctrl_initiated);
        });
        crate::components::overlays::clear_global_tooltip();
        cx.notify();
    }

    /// Arm a header press so pointer travel promotes it into a pane drag.
    pub fn arm_pane_drag(
        &mut self,
        path: Vec<usize>,
        channel: String,
        start_pos: Point<Pixels>,
        cx: &mut Context<Self>,
    ) {
        if self.app_state.read(cx).active_tab().is_locked {
            return;
        }
        let tab_id = self.app_state.read(cx).active_tab().id.clone();
        self.split_layout.update(cx, |layout, _| {
            layout.arm_pane_drag(tab_id, path, channel, start_pos);
        });
        cx.notify();
    }

    /// End pane drag and execute the drop operation
    pub fn end_pane_drag(&mut self, cx: &mut Context<Self>) {
        self.cancel_stale_pointer_drags(cx);
        let result = self
            .split_layout
            .update(cx, |layout, _| layout.end_pane_drag());
        let Some((drag, target)) = result else {
            cx.notify();
            return;
        };

        if drag.source_path == target.target_path {
            cx.notify();
            return;
        }

        let source_path = drag.source_path;
        let target_path = target.target_path;
        let zone = target.zone;

        self.app_state.update(cx, |state, _cx| {
            let workspace = state.active_tab_mut();
            if target.root_edge {
                workspace.move_split_to_edge(&source_path, zone);
                return;
            }
            match zone {
                DropZone::Center => {
                    workspace.swap_splits(&source_path, &target_path);
                }
                DropZone::Top | DropZone::Bottom | DropZone::Left | DropZone::Right => {
                    workspace.move_split(&source_path, &target_path, zone);
                }
            }
        });

        self.save_workspace_layout(cx);
        cx.notify();
    }

    /// End tab drag and reorder if needed
    pub fn end_tab_drag(&mut self, cx: &mut Context<Self>) {
        self.cancel_stale_pointer_drags(cx);
        let is_pending = self.tab_bar_renderer.read(cx).is_tab_drag_pending();
        let is_dragging = self.tab_bar_renderer.read(cx).is_tab_dragging();

        if is_pending && !is_dragging {
            self.tab_bar_renderer.update(cx, |r, _| r.cancel_tab_drag());
            cx.notify();
            return;
        }

        if !is_dragging {
            return;
        }

        let drag_result = {
            let app_state = self.app_state.clone();
            self.tab_bar_renderer.update(cx, |r, cx| {
                let state = app_state.read(cx);
                r.complete_tab_drag(state.tabs.iter().map(|tab| tab.id.as_str()))
            })
        };

        if let Some((source_idx, target)) = drag_result
            && let Some(moves) = TabManager::compute_reorder(source_idx, &target)
        {
            self.app_state.update(cx, |state, _cx| {
                for (from, to) in moves {
                    state.move_tab(from, to);
                }
            });
            self.save_tabs(cx);
        }

        cx.notify();
    }

    /// Give every child of the split at `path` an equal share.
    pub fn reset_split_ratios(&mut self, path: &[usize], cx: &mut Context<Self>) {
        let changed = self
            .app_state
            .update(cx, |state, _cx| state.active_tab_mut().reset_ratios(path));
        if changed {
            self.save_workspace_layout(cx);
            cx.notify();
        }
    }

    /// Add a new empty split relative to the target
    pub fn add_empty_split(
        &mut self,
        target_path: Vec<usize>,
        direction: AddSplitDirection,
        cx: &mut Context<Self>,
    ) {
        if self.refuse_locked_split_layout(cx) {
            return;
        }
        let new_path = self.app_state.update(cx, |state, _cx| {
            state
                .active_tab_mut()
                .insert_placeholder_split_adjacent_to_path(&target_path, direction)
        });

        if let Some(path) = new_path {
            self.set_active_split(path, cx);
        }
        self.save_workspace_layout(cx);
        cx.notify();
    }

    /// Render the drag ghost when dragging a split pane
    fn render_drag_ghost(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let layout = self.split_layout.read(cx);
        let pane_drag = layout.pane_drag().cloned();
        let cursor_position = layout.cursor_position();

        self.split_tree_renderer.update(cx, |renderer, _cx| {
            renderer.render_drag_ghost(pane_drag.as_ref(), cursor_position)
        })
    }

    /// Render drop zone overlay when dragging over a split
    fn render_drop_zone_overlay(
        &self,
        zone: Option<DropZone>,
        width: f32,
        height: f32,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        self.split_tree_renderer.update(cx, |renderer, _cx| {
            renderer.render_drop_zone_overlay(zone, width, height)
        })
    }

    /// Render the Ctrl+Alt split overlay for the hovered split.
    ///
    /// Draws a translucent scrim over the split, directional add-split buttons
    /// anchored to the four edges, a centered move handle that starts a drag,
    /// and a half-pane preview of where the new split will land while a
    /// directional button is hovered. Edge buttons hide on panes too small to
    /// show them.
    fn render_add_split_buttons(
        &self,
        path: Vec<usize>,
        pane_width: f32,
        pane_height: f32,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        /// The horizontal/vertical buttons hide below this pane size.
        const MIN_BUTTON_AXIS_PX: f32 = 150.0;
        const BUTTON_PX: f32 = 32.0;
        const EDGE_INSET_PX: f32 = 12.0;

        let path_key = SplitLayoutManager::path_key(&path);
        let wide_enough = pane_width > MIN_BUTTON_AXIS_PX;
        let high_enough = pane_height > MIN_BUTTON_AXIS_PX;

        let center_x = (pane_width - BUTTON_PX) / 2.0;
        let center_y = (pane_height - BUTTON_PX) / 2.0;
        let hovered_direction = self.split_layout.read(cx).hovered_add_direction();

        let mut overlay = modal_backdrop_style()
            .id(ElementId::Name(format!("split-overlay-{path_key}").into()))
            .bg(SplitLayoutManager::split_overlay_scrim())
            .cursor_pointer()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation());

        if let Some(direction) = hovered_direction {
            let (left, top, width, height) =
                SplitLayoutManager::add_split_preview_rect(direction, pane_width, pane_height);
            let accent = colors::accent();
            overlay = overlay.child(
                div()
                    .absolute()
                    .left(px(left))
                    .top(px(top))
                    .w(px(width))
                    .h(px(height))
                    .bg(accent.opacity(0.18))
                    .border_1()
                    .border_color(accent.opacity(0.85)),
            );
        }

        let mut buttons: Vec<(AddSplitDirection, f32, f32)> = Vec::with_capacity(4);
        if high_enough {
            buttons.push((AddSplitDirection::Top, center_x, EDGE_INSET_PX));
            buttons.push((
                AddSplitDirection::Bottom,
                center_x,
                pane_height - BUTTON_PX - EDGE_INSET_PX,
            ));
        }
        if wide_enough {
            buttons.push((AddSplitDirection::Left, EDGE_INSET_PX, center_y));
            buttons.push((
                AddSplitDirection::Right,
                pane_width - BUTTON_PX - EDGE_INSET_PX,
                center_y,
            ));
        }

        for (direction, left, top) in buttons {
            overlay = overlay.child(self.render_add_split_button(
                path.clone(),
                direction,
                left,
                top,
                BUTTON_PX,
                cx,
            ));
        }

        overlay.child(self.render_split_move_handle(path, center_x, center_y, BUTTON_PX, cx))
    }

    /// Render a single edge-anchored add-split button.
    ///
    /// Hovering publishes the direction so the overlay can paint its half-pane
    /// preview underneath every button, which a sibling `group_hover` cannot do.
    fn render_add_split_button(
        &self,
        path: Vec<usize>,
        direction: AddSplitDirection,
        left: f32,
        top: f32,
        size: f32,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let icon = match direction {
            AddSplitDirection::Top => "↑",
            AddSplitDirection::Bottom => "↓",
            AddSplitDirection::Left => "←",
            AddSplitDirection::Right => "→",
        };

        let path_key = SplitLayoutManager::path_key(&path);

        div()
            .id(ElementId::Name(
                format!("add-split-{direction:?}-{path_key}").into(),
            ))
            .absolute()
            .left(px(left))
            .top(px(top))
            .w(px(size))
            .h(px(size))
            .bg(colors::accent())
            .border_1()
            .border_color(colors::accent_hover())
            .rounded(Radii::full())
            .cursor_pointer()
            .flex()
            .items_center()
            .justify_center()
            .hover(|s| s.bg(colors::accent_hover()))
            .shadow_md()
            .on_hover(cx.listener(move |this, hovered: &bool, _window, cx| {
                let next = hovered.then_some(direction);
                let changed = this.split_layout.update(cx, |layout, _| {
                    if *hovered || layout.hovered_add_direction() == Some(direction) {
                        layout.set_hovered_add_direction(next)
                    } else {
                        false
                    }
                });
                if changed {
                    cx.notify();
                }
            }))
            .on_activate(cx.listener(move |this, _event, _window, cx| {
                this.split_layout.update(cx, |layout, _| {
                    layout.dismiss_add_split_overlay_until_release();
                });
                this.add_empty_split(path.clone(), direction, cx);
                cx.stop_propagation();
            }))
            .child(
                div()
                    .text_sm()
                    .font_weight(FontWeight::BOLD)
                    .text_color(colors::text_on_accent_contrast())
                    .child(icon),
            )
    }

    /// Center "move" handle of the Ctrl+Alt overlay: press it to drag the split.
    fn render_split_move_handle(
        &self,
        path: Vec<usize>,
        left: f32,
        top: f32,
        size: f32,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let path_key = SplitLayoutManager::path_key(&path);
        let channel = self
            .app_state
            .read(cx)
            .active_tab()
            .layout
            .get_channel_at_path(&path)
            .unwrap_or_default();
        let draggable = !is_placeholder(&channel);

        div()
            .id(ElementId::Name(format!("split-move-{path_key}").into()))
            .absolute()
            .left(px(left))
            .top(px(top))
            .w(px(size))
            .h(px(size))
            .bg(colors::bg_elevated())
            .border_1()
            .border_color(colors::border())
            .rounded(Radii::full())
            .flex()
            .items_center()
            .justify_center()
            .shadow_md()
            .when(draggable, |handle| {
                handle
                    .cursor(CursorStyle::OpenHand)
                    .hover(|s| s.bg(colors::bg_hover()))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _event, _window, cx| {
                            this.start_pane_drag(path.clone(), channel.clone(), false, cx);
                            cx.stop_propagation();
                        }),
                    )
            })
            .when(!draggable, |handle| handle.opacity(0.4))
            .child(
                div()
                    .text_sm()
                    .font_weight(FontWeight::BOLD)
                    .text_color(colors::text_primary())
                    .child("✥"),
            )
    }

    fn render_settings_modal(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let settings_state = self.overlay_manager.read(cx).settings_modal.clone();
        let is_open = settings_state.read(cx).is_open;

        if !is_open {
            return div().into_any_element();
        }

        if settings_state.read(cx).needs_login_modal() {
            let login_modal_state = self.overlay_manager.read(cx).login_modal.clone();
            settings_state.update(cx, |state, _| {
                state.attach_login_modal(&login_modal_state);
            });
        }

        crate::components::overlays::clear_global_tooltip();
        AnyView::from(settings_state).into_any_element()
    }

    /// Render split settings dropdown as app-level overlay
    /// Uses split_hit_areas for dynamic positioning that follows resize
    fn render_settings_dropdown_backdrop(
        &mut self,
        chrome_height: f32,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let Some((path_key, channel)) =
            self.overlay_manager.read(cx).open_settings_dropdown.clone()
        else {
            return div().into_any_element();
        };

        crate::components::overlays::clear_global_tooltip();

        let bounds = self.split_layout.read(cx).hit_area_for(&path_key);
        let Some(bounds) = bounds else {
            self.overlay_manager
                .update(cx, |overlays, _| overlays.close_all_split_settings());
            return div().into_any_element();
        };

        let compact_split_header = backend::settings::get_compact_split_header();
        let header_height = main_split_header_height(compact_split_header);
        let header_collapsed = self
            .app_state
            .read(cx)
            .active_tab()
            .layout
            .header_collapsed_at_path(&path_key.to_vec())
            .unwrap_or(false);

        let custom_player_path = crate::backend::settings::get_use_custom_player()
            .then(|| {
                crate::backend::settings::get_custom_player_path()
                    .filter(|path| !path.trim().is_empty())
            })
            .flatten();
        let shortcut_for = |action: crate::backend::hotkeys::HotkeyAction| {
            crate::backend::hotkeys::get_binding(action)
                .map(|binding| SharedString::from(binding.to_display()))
        };

        let deleted_mode_override = self
            .app_state
            .read(cx)
            .active_tab()
            .layout
            .deleted_mode_at_path(&path_key.to_vec());
        let is_whispers = backend::whispers::is_whispers_channel(&channel);
        let mut rows: Vec<SplitMenuRow> = vec![
            SplitMenuRow {
                action: SplitMenuAction::ToggleHeader,
                label: if header_collapsed {
                    "Show header".into()
                } else {
                    "Hide header".into()
                },
                shortcut: None,
            },
            SplitMenuRow {
                action: SplitMenuAction::AddSplit,
                label: "Add split to right".into(),
                shortcut: shortcut_for(crate::backend::hotkeys::HotkeyAction::NewSplit),
            },
            SplitMenuRow {
                action: SplitMenuAction::ChangeChannel,
                label: "Change channel".into(),
                shortcut: shortcut_for(crate::backend::hotkeys::HotkeyAction::ChangeChannel),
            },
            SplitMenuRow {
                action: SplitMenuAction::LiveFollowedChannels,
                label: "Live followed channels…".into(),
                shortcut: None,
            },
        ];
        if !is_whispers {
            rows.extend([
                SplitMenuRow {
                    action: SplitMenuAction::OpenChatters,
                    label: "Open chatters".into(),
                    shortcut: None,
                },
                SplitMenuRow {
                    action: SplitMenuAction::OpenChannelPopup,
                    label: "Open channel popup".into(),
                    shortcut: None,
                },
                SplitMenuRow {
                    action: SplitMenuAction::ModLog,
                    label: "Mod Log".into(),
                    shortcut: None,
                },
                SplitMenuRow {
                    action: SplitMenuAction::CycleDeletedMode,
                    label: match deleted_mode_override {
                        Some(mode) => format!("Deleted: {}", deleted_mode_label(mode)).into(),
                        None => format!(
                            "Deleted: Default ({})",
                            deleted_mode_label(backend::settings::get_deleted_message_mode())
                        )
                        .into(),
                    },
                    shortcut: None,
                },
            ]);
        }
        if !is_whispers && custom_player_path.is_some() {
            rows.push(SplitMenuRow {
                action: SplitMenuAction::OpenCustomPlayer,
                label: "Open with Custom Player".into(),
                shortcut: None,
            });
        }
        if !is_whispers {
            rows.push(SplitMenuRow {
                action: SplitMenuAction::OpenStreamlink,
                label: "Open with Streamlink".into(),
                shortcut: None,
            });
        }
        rows.push(SplitMenuRow {
            action: SplitMenuAction::CloseSplit,
            label: "Close split".into(),
            shortcut: shortcut_for(crate::backend::hotkeys::HotkeyAction::CloseSplit),
        });

        let viewport = window.viewport_size();
        let (intrinsic_width, shortcut_column) = measure_split_menu_width(&rows, window);
        let dropdown_fit = crate::utils::fit_overlay_to_viewport(
            viewport,
            intrinsic_width.clamp(SPLIT_MENU_MIN_WIDTH_PX, SPLIT_MENU_MAX_WIDTH_PX),
            SPLIT_MENU_MAX_HEIGHT_PX,
            8.0,
        );
        let dropdown_width = dropdown_fit.width;
        let show_shortcuts = shortcut_column <= 0.0
            || dropdown_width
                >= MenuMetrics::ITEM_PADDING_X * 2.0
                    + SPLIT_MENU_BORDER_PX
                    + SPLIT_MENU_MIN_LABEL_PX
                    + shortcut_column;
        let horizontal_inset = if header_collapsed { 8.0 } else { 28.0 };
        let vertical_offset = if header_collapsed {
            40.0
        } else {
            header_height + 4.0
        };
        let dropdown_x: f32 =
            (bounds.origin.x + bounds.size.width - px(dropdown_width + horizontal_inset)).into();
        let dropdown_y: f32 = (bounds.origin.y + px(vertical_offset)).into();

        let clamped_x = crate::utils::clamp_overlay_axis(
            dropdown_x,
            dropdown_width,
            f32::from(viewport.width),
            dropdown_fit.margin_x,
        );
        let clamped_y = crate::utils::clamp_overlay_axis(
            dropdown_y.max(chrome_height + dropdown_fit.margin_y),
            dropdown_fit.height,
            f32::from(viewport.height),
            dropdown_fit.margin_y,
        );

        div()
            .id("settings-dropdown-overlay")
            .absolute()
            .top_0()
            .left_0()
            .right_0()
            .bottom_0()
            .occlude()
            .on_mouse_move(|_, _, cx| cx.stop_propagation())
            .on_mouse_up(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_mouse_up(MouseButton::Right, |_, _, cx| cx.stop_propagation())
            .on_mouse_up(MouseButton::Middle, |_, _, cx| cx.stop_propagation())
            .on_scroll(|_, _, cx| cx.stop_propagation())
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    this.overlay_manager
                        .update(cx, |o, _| o.close_all_split_settings());
                    cx.notify();
                    cx.stop_propagation();
                }),
            )
            .on_mouse_down(MouseButton::Right, |_, _, cx| cx.stop_propagation())
            .on_mouse_down(MouseButton::Middle, |_, _, cx| cx.stop_propagation())
            .child({
                let menu = self.chrome_menu_host(cx);
                let nav = menu.nav.clone();
                let app = cx.entity();
                let actions: Vec<MenuAction> = rows
                    .iter()
                    .map(|row| {
                        let path = path_key.to_vec();
                        let ch = channel.clone();
                        let player_path = custom_player_path.clone();
                        let action = row.action;
                        let app = app.clone();
                        Rc::new(move |window: &mut Window, cx: &mut App| {
                            app.update(cx, |this, cx| {
                                match action {
                                    SplitMenuAction::ToggleHeader => {
                                        this.set_split_header_collapsed(
                                            path.clone(),
                                            !header_collapsed,
                                            cx,
                                        );
                                    }
                                    SplitMenuAction::AddSplit => {
                                        this.add_empty_split(
                                            path.clone(),
                                            AddSplitDirection::Right,
                                            cx,
                                        );
                                    }
                                    SplitMenuAction::ChangeChannel => {
                                        this.set_active_split(path.clone(), cx);
                                        let _ = this.start_change_active_split_channel(cx);
                                    }
                                    SplitMenuAction::LiveFollowedChannels => {
                                        this.overlay_manager
                                            .update(cx, |o, _| o.close_all_split_settings());
                                        let source = this
                                            .app_state
                                            .read(cx)
                                            .active_tab()
                                            .layout
                                            .get_split_id_at_path(&path);
                                        this.open_live_followed_switcher(source, window, cx);
                                        return;
                                    }
                                    SplitMenuAction::OpenChatters => {
                                        backend::open_chatter_list_async(ch.clone());
                                    }
                                    SplitMenuAction::OpenChannelPopup => {
                                        crate::components::chat::open_channel_popup(
                                            ch.clone(),
                                            this.app_state.clone(),
                                            cx,
                                        );
                                    }
                                    SplitMenuAction::ModLog => this.open_mod_log(cx),
                                    SplitMenuAction::CycleDeletedMode => {
                                        this.set_split_deleted_mode(
                                            path.clone(),
                                            next_deleted_mode(deleted_mode_override),
                                            cx,
                                        );
                                        // Stay open so the label shows the new mode.
                                        return;
                                    }
                                    SplitMenuAction::OpenCustomPlayer => {
                                        if let Some(player_path) = player_path.clone() {
                                            backend::commands::launch_stream_player(
                                                &ch,
                                                backend::commands::StreamPlayer::Custom(
                                                    player_path,
                                                ),
                                            );
                                        }
                                    }
                                    SplitMenuAction::OpenStreamlink => {
                                        backend::commands::launch_stream_player(
                                            &ch,
                                            backend::commands::StreamPlayer::Streamlink,
                                        );
                                    }
                                    SplitMenuAction::CloseSplit => {
                                        this.overlay_manager
                                            .update(cx, |o, _| o.close_all_split_settings());
                                        this.close_pane(path.clone(), cx);
                                        cx.notify();
                                        return;
                                    }
                                }
                                this.overlay_manager
                                    .update(cx, |o, _| o.close_all_split_settings());
                                cx.notify();
                            });
                        }) as MenuAction
                    })
                    .collect();
                menu_panel()
                    .id("split-settings-dropdown-panel")
                    .absolute()
                    .left(px(clamped_x))
                    .top(px(clamped_y))
                    .w(px(dropdown_width))
                    .max_h(px(dropdown_fit.height))
                    .overflow_y_scroll()
                    .overflow_x_hidden()
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .menu_keyboard(&menu.focus, &nav, actions.clone(), menu.close_handler())
                    .children(rows.into_iter().zip(actions).enumerate().map(
                        |(row_index, (row, action))| {
                            menu_item(("split-menu-item", row_index), row_index, &nav, action)
                                .child(
                                    div()
                                        .min_w_0()
                                        .flex_1()
                                        .overflow_hidden()
                                        .whitespace_nowrap()
                                        .text_ellipsis()
                                        .text_size(px(MenuMetrics::ITEM_TEXT))
                                        .text_color(colors::text_primary())
                                        .child(row.label),
                                )
                                .when_some(
                                    row.shortcut.filter(|_| show_shortcuts),
                                    |item, shortcut| {
                                        item.child(
                                            div()
                                                .flex_shrink_0()
                                                .whitespace_nowrap()
                                                .text_size(px(MenuMetrics::SECONDARY_TEXT))
                                                .text_color(colors::text_muted())
                                                .child(shortcut),
                                        )
                                    },
                                )
                        },
                    ))
            })
            .into_any_element()
    }

    fn render_notification_event_row(
        event: backend::TwitchEvent,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let title = backend::twitch_event_title(&event.event_type).to_string();
        let body = backend::format_twitch_event_text(&event);
        let event_timestamp = event.timestamp;
        let dismiss_timestamp = event.timestamp;
        let timestamp = crate::utils::format_sidebar_timestamp(event.timestamp);
        let event_channel = event.channel.clone();
        let automod_message_id = pending_automod_message_id(&event);
        let suspicious_user_target = suspicious_user_notification_target(&event);
        let channel = if event.channel.is_empty() {
            "Twitch".to_string()
        } else {
            event.channel.to_string()
        };
        let copy_text = crate::components::sidebars::format_activity_copy_text(
            "Event", &title, &channel, &body,
        );
        let copy_action_id = {
            let mut hasher = FxHasher::default();
            event_timestamp.hash(&mut hasher);
            copy_text.hash(&mut hasher);
            format!("activity-event-{}-copy", hasher.finish())
        };

        let row = div()
            .id(SharedString::from(format!(
                "activity-event-{event_timestamp}"
            )))
            .w_full()
            .px_3()
            .py_2()
            .border_b_1()
            .border_color(colors::border())
            .on_right_click(cx.listener(move |_this, _, _window, cx| {
                backend::remove_recent_twitch_event(event_timestamp);
                cx.notify();
            }))
            .child(
                div()
                    .w_full()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .child(
                        div()
                            .text_xs()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(colors::text_primary())
                            .child(title),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .text_xs()
                            .text_color(colors::text_muted())
                            .child(div().max_w(px(120.0)).truncate().child(channel))
                            .when(!timestamp.is_empty(), |s| {
                                s.child(
                                    div()
                                        .flex_shrink_0()
                                        .text_color(colors::text_muted())
                                        .child(timestamp),
                                )
                            })
                            .child(crate::components::sidebars::render_activity_copy_action(
                                copy_action_id,
                                copy_text,
                            ))
                            .child(
                                div()
                                    .id(SharedString::from(format!(
                                        "dismiss-activity-event-{dismiss_timestamp}"
                                    )))
                                    .role(Role::Button)
                                    .aria_label("Dismiss event")
                                    .on_hover(crate::components::overlays::text_tooltip_on_hover(
                                        format!("dismiss-activity-event-{dismiss_timestamp}"),
                                        "Dismiss".to_string(),
                                    ))
                                    // 24 px hit area in the 18 px slot it always had.
                                    .size(px(Spacing::XXL))
                                    .m(px(-Spacing::XS_PLUS / 2.0))
                                    .rounded(Radii::sm())
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .cursor_pointer()
                                    .hover(|s| s.bg(colors::bg_hover()))
                                    .on_activate(cx.listener(move |_this, _event, _window, cx| {
                                        backend::remove_recent_twitch_event(dismiss_timestamp);
                                        cx.notify();
                                    }))
                                    .child(
                                        div()
                                            .text_xs()
                                            .font_weight(FontWeight::BOLD)
                                            .text_color(colors::text_muted())
                                            .child(crate::icons::CLOSE_GLYPH),
                                    ),
                            ),
                    ),
            )
            .child(
                div()
                    .pt_1()
                    .text_sm()
                    .text_color(colors::text_secondary())
                    .child(body),
            )
            .when_some(automod_message_id, move |row, message_id| {
                row.child(
                    div()
                        .pt_2()
                        .flex()
                        .gap_2()
                        .child(automod_action_button(
                            "Allow",
                            message_id.clone(),
                            true,
                            event_timestamp,
                        ))
                        .child(automod_action_button(
                            "Deny",
                            message_id,
                            false,
                            event_timestamp,
                        )),
                )
            })
            .when_some(
                suspicious_user_target,
                move |row, (target_channel, user_login, user_id, status)| {
                    let is_restricted = status.eq_ignore_ascii_case("restricted");
                    let is_monitored = status.eq_ignore_ascii_case("active_monitoring")
                        || status.eq_ignore_ascii_case("monitored");
                    let actions = div().pt_2().flex().gap_2();
                    let actions = if is_restricted {
                        actions
                            .child(suspicious_user_action_button(
                                "Monitor",
                                target_channel.clone(),
                                user_login.clone(),
                                user_id.clone(),
                                backend::commands::helix::SuspiciousUserAction::Monitor,
                                event_timestamp,
                            ))
                            .child(suspicious_user_action_button(
                                "Clear",
                                target_channel,
                                user_login,
                                user_id,
                                backend::commands::helix::SuspiciousUserAction::Clear,
                                event_timestamp,
                            ))
                    } else if is_monitored {
                        actions
                            .child(suspicious_user_action_button(
                                "Restrict",
                                target_channel.clone(),
                                user_login.clone(),
                                user_id.clone(),
                                backend::commands::helix::SuspiciousUserAction::Restrict,
                                event_timestamp,
                            ))
                            .child(suspicious_user_action_button(
                                "Clear",
                                target_channel,
                                user_login,
                                user_id,
                                backend::commands::helix::SuspiciousUserAction::Clear,
                                event_timestamp,
                            ))
                    } else {
                        actions
                            .child(suspicious_user_action_button(
                                "Monitor",
                                target_channel.clone(),
                                user_login.clone(),
                                user_id.clone(),
                                backend::commands::helix::SuspiciousUserAction::Monitor,
                                event_timestamp,
                            ))
                            .child(suspicious_user_action_button(
                                "Restrict",
                                target_channel,
                                user_login,
                                user_id,
                                backend::commands::helix::SuspiciousUserAction::Restrict,
                                event_timestamp,
                            ))
                    };
                    row.child(actions)
                },
            );

        if event_channel.is_empty() {
            row.into_any_element()
        } else {
            row.cursor_pointer()
                .hover(|s| s.bg(colors::bg_hover()))
                .on_activate(cx.listener(move |this, _, _window, cx| {
                    this.focus_notification_channel(&event_channel, cx);
                    this.app_state.update(cx, |state, cx| {
                        state.close_activity(cx);
                    });
                }))
                .into_any_element()
        }
    }

    fn focus_notification_channel(&mut self, channel: &str, cx: &mut Context<Self>) -> bool {
        let channel = channel.trim_start_matches('#').to_lowercase();
        if channel.is_empty() {
            return false;
        }

        let target = {
            let app_state = self.app_state.read(cx);
            find_open_tab_channel_target(app_state, &channel)
        };

        let Some((tab_index, path, _resolved_channel)) = target else {
            crate::log_ui!(
                warn,
                "Notification target channel is not visible in any open tab: {}",
                channel
            );
            return false;
        };

        self.activate_tab_split(tab_index, path, cx);
        true
    }

    fn render_activity_events_content(
        &mut self,
        notification_filter: backend::ui_actions::NotificationFilter,
        mut events: Vec<backend::TwitchEvent>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (description, empty_text) = match notification_filter {
            backend::ui_actions::NotificationFilter::All => {
                ("Recent channel events", "No recent channel events")
            }
            backend::ui_actions::NotificationFilter::Automod => {
                ("AutoMod review", "No recent AutoMod messages")
            }
        };
        if matches!(
            notification_filter,
            backend::ui_actions::NotificationFilter::Automod
        ) {
            events.retain(is_automod_notification_event);
        }
        let has_events = !events.is_empty();
        let content = if events.is_empty() {
            div()
                .flex_1()
                .min_h_0()
                .w_full()
                .p_6()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .child(
                    div()
                        .text_sm()
                        .text_color(colors::text_muted())
                        .child(empty_text),
                )
                .into_any_element()
        } else {
            div()
                .id("activity-event-list")
                .flex_1()
                .min_h_0()
                .w_full()
                .overflow_y_scroll()
                .children(
                    events
                        .into_iter()
                        .map(|event| Self::render_notification_event_row(event, cx)),
                )
                .into_any_element()
        };

        div()
            .flex_1()
            .min_h_0()
            .w_full()
            .flex()
            .flex_col()
            .child(
                div()
                    .h(px(38.0))
                    .w_full()
                    .px_3()
                    .flex_shrink_0()
                    .border_b_1()
                    .border_color(colors::border())
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_xs()
                            .text_color(colors::text_muted())
                            .child(description),
                    )
                    .when(has_events, |toolbar| {
                        toolbar.child(
                            div()
                                .id("clear-activity-events")
                                .flex_shrink_0()
                                .text_xs()
                                .text_color(colors::text_muted())
                                .cursor_pointer()
                                .hover(|style| style.text_color(colors::text_secondary()))
                                .on_activate(cx.listener(move |_this, _, _, cx| {
                                    match notification_filter {
                                        backend::ui_actions::NotificationFilter::All => {
                                            backend::clear_recent_twitch_events();
                                        }
                                        backend::ui_actions::NotificationFilter::Automod => {
                                            backend::clear_recent_automod_events();
                                        }
                                    }
                                    cx.notify();
                                    cx.stop_propagation();
                                }))
                                .child("Clear"),
                        )
                    }),
            )
            .child(content)
            .into_any_element()
    }

    /// Render login modal overlay
    fn render_login_modal(
        &self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let login_modal_state = self.overlay_manager.read(cx).login_modal.clone();
        let is_open = login_modal_state.read(cx).is_open;

        if !is_open {
            return div().into_any_element();
        }

        LoginModal::render_with_entity(login_modal_state, window.viewport_size(), cx)
    }
}

impl Focusable for EchoApp {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}
