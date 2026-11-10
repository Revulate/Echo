//! Hotkeys settings section
//!
//! Displays all rebindable hotkey actions organized by category.
//! Supports click-to-rebind with key capture mode.

use crate::backend::hotkeys::{
    self, CHAT_ACTIONS, CustomHotkeyAction, HotkeyAction, MODERATION_ACTIONS, NAVIGATION_ACTIONS,
    OTHER_ACTIONS, SEARCH_ACTIONS, custom_hotkeys, get_capturing_storage_key,
};
use crate::backend::lock_utils::MutexExt;
use crate::components::settings::editor_state::{EditorSnapshot, EditorState};
use crate::components::settings::settings_navigation::{
    SettingDescriptor, SettingsSectionId, SettingsSubsectionId,
};
use crate::components::settings::settings_page::SettingsPage;
use crate::components::settings::widgets::{
    SettingsMetrics, SettingsPageUi, control_focus, empty_state, labelled_toggle,
    quiet_action_button, setting_reset, setting_target, settings_group, settings_group_contents,
    settings_subheading, settings_summary_disclosure,
};
use crate::components::ui::ButtonExt;
use crate::components::ui::input::{Input, InputEvent, InputState, normalize_single_line_text};
use crate::theme::colors;
use crate::theme::tokens::{Radii, RadiiPixels};
use gpui::prelude::FluentBuilder;
use gpui::*;
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::{LazyLock, Mutex};
use uuid::Uuid;

static CAPTURE_REJECTION: Mutex<Option<String>> = Mutex::new(None);
static HOTKEY_DEFAULTS: LazyLock<HashMap<String, hotkeys::HotkeyBinding>> =
    LazyLock::new(hotkeys::default_hotkeys);

const SHORTCUTS_EDITOR_DISCLOSURE: &str = "shortcuts-editor";

pub const HOTKEYS_EDITOR_SETTINGS: &[SettingDescriptor] = &[SettingDescriptor {
    id: "hotkeys.editor",
    label: "Keyboard shortcuts",
    aliases: &["shortcut editor", "keyboard bindings"],
    control_id: "shortcuts-editor",
    disclosures: &[SHORTCUTS_EDITOR_DISCLOSURE],
}];

/// Keys rejected for new captures: character/space typing keys, plus plain Tab.
/// Saved Tab bindings remain usable outside composer/shared-Input focus.
pub fn is_plain_typing_binding(binding: &hotkeys::HotkeyBinding) -> bool {
    !binding.ctrl
        && !binding.alt
        && (binding.key.chars().count() == 1
            || binding.key == "space"
            || binding.key.eq_ignore_ascii_case("tab"))
}

/// Refuse to record a plain typing key, or a combination that `types_text` on
/// the current layout (AltGr characters); capture stays active so the user can
/// press a different combination.
pub fn reject_plain_typing_capture(binding: &hotkeys::HotkeyBinding, types_text: bool) -> bool {
    let rejection = if binding.key.eq_ignore_ascii_case("tab") && !binding.ctrl && !binding.alt {
        Some(
            "Tab and Shift+Tab are reserved for completion and focus navigation. Add Ctrl or Alt."
                .to_string(),
        )
    } else if is_plain_typing_binding(binding) {
        Some(format!(
            "{} types into chat, so it can't be a shortcut. Add Ctrl or Alt.",
            binding.to_display()
        ))
    } else if types_text {
        Some(format!(
            "{} types a character on this keyboard layout, so it can't be a shortcut.",
            binding.to_display()
        ))
    } else {
        None
    };
    let rejected = rejection.is_some();
    *CAPTURE_REJECTION.lock_or_recover() = rejection;
    rejected
}

pub fn clear_capture_rejection() {
    *CAPTURE_REJECTION.lock_or_recover() = None;
}

fn snapshot_binding(
    bindings: &HashMap<String, hotkeys::HotkeyBinding>,
    storage_key: &str,
) -> Option<hotkeys::HotkeyBinding> {
    bindings
        .get(storage_key)
        .filter(|binding| !binding.key.trim().is_empty())
        .cloned()
}

fn typing_binding_detail(
    bindings: &HashMap<String, hotkeys::HotkeyBinding>,
    storage_key: &str,
) -> Option<String> {
    snapshot_binding(bindings, storage_key)
        .filter(is_plain_typing_binding)
        .map(|binding| {
            if binding.key.eq_ignore_ascii_case("tab") {
                "Reserved for completion and focus navigation while a composer or text field is focused; this saved binding still works elsewhere.".to_string()
            } else {
                format!("{} types into chat and never triggers this. Add Ctrl or Alt.", binding.to_display())
            }
        })
}

const HOTKEYS_NAVIGATION: SettingDescriptor = SettingDescriptor {
    id: "hotkeys.navigation",
    label: "Navigation hotkeys",
    aliases: &["tab", "split", "switch", "next", "previous"],
    control_id: "hotkeys-navigation",
    disclosures: &[SHORTCUTS_EDITOR_DISCLOSURE],
};

macro_rules! hotkey_settings {
    ($category:expr; $(($variant:ident, $storage_key:literal, $label:literal)),* $(,)?) => {
        &[
            $category,
            $(SettingDescriptor {
                id: concat!("hotkeys.", $storage_key),
                label: $label,
                aliases: &[$storage_key],
                control_id: concat!("hotkey-bind-", stringify!($variant)),
                disclosures: &[SHORTCUTS_EDITOR_DISCLOSURE],
            },)*
        ]
    };
}

/// Hotkeys › Navigation.
pub const HOTKEYS_NAVIGATION_SETTINGS: &[SettingDescriptor] = hotkey_settings!(
    HOTKEYS_NAVIGATION;
    (ToggleSettings, "toggleSettings", "Open settings"),
    (ToggleAccountSwitcher, "toggleAccountSwitcher", "Toggle account switcher"),
    (ToggleNotifications, "toggleNotifications", "Toggle notifications"),
    (ToggleSearch, "toggleSearch", "Open search"),
    (ToggleGlobalSearch, "toggleGlobalSearch", "Open global search"),
    (ToggleWhispers, "toggleWhispers", "Toggle whispers"),
    (ToggleMentions, "toggleMentions", "Toggle mentions"),
    (NewTab, "newTab", "New tab"),
    (NewSplit, "newSplit", "New split"),
    (NextTab, "nextTab", "Next tab"),
    (PrevTab, "prevTab", "Previous tab"),
    (CloseSplit, "closeSplit", "Close split"),
    (CloseTab, "closeTab", "Close tab"),
    (MoveTabLeft, "moveTabLeft", "Move tab left"),
    (MoveTabRight, "moveTabRight", "Move tab right"),
    (OpenTabMenu, "openTabMenu", "Open tab menu"),
    (RenameTab, "renameTab", "Rename tab"),
    (SelectTab1, "selectTab1", "Select tab 1"),
    (SelectTab2, "selectTab2", "Select tab 2"),
    (SelectTab3, "selectTab3", "Select tab 3"),
    (SelectTab4, "selectTab4", "Select tab 4"),
    (SelectTab5, "selectTab5", "Select tab 5"),
    (SelectTab6, "selectTab6", "Select tab 6"),
    (SelectTab7, "selectTab7", "Select tab 7"),
    (SelectTab8, "selectTab8", "Select tab 8"),
    (SelectTab9, "selectTab9", "Select last tab"),
    (PopupActiveSplit, "popupActiveSplit", "Popup active split"),
    (PopupActiveTab, "popupActiveTab", "Popup active tab"),
    (ZoomIn, "zoomIn", "Zoom in"),
    (ZoomOut, "zoomOut", "Zoom out"),
    (ResetZoom, "resetZoom", "Reset zoom"),
    (ToggleTabBarVisibility, "toggleTabBarVisibility", "Toggle tab bar visibility"),
    (ToggleLiveTabsOnly, "toggleLiveTabsOnly", "Toggle live tabs only"),
    (ToggleSidebar, "toggleSidebar", "Toggle sidebar"),
);

const HOTKEYS_CHAT: SettingDescriptor = SettingDescriptor {
    id: "hotkeys.chat",
    label: "Chat hotkeys",
    aliases: &["send", "reply", "clear", "scroll"],
    control_id: "hotkeys-chat",
    disclosures: &[SHORTCUTS_EDITOR_DISCLOSURE],
};

/// Hotkeys › Chat.
pub const HOTKEYS_CHAT_SETTINGS: &[SettingDescriptor] = hotkey_settings!(
    HOTKEYS_CHAT;
    (ChangeChannel, "changeChannel", "Change channel"),
    (FocusInput, "focusInput", "Focus input"),
    (SendMessage, "sendMessage", "Send message"),
    (SendMessageKeepInput, "sendMessageKeepInput", "Send message and keep input"),
    (OpenEmotePicker, "openEmotePicker", "Open emote picker"),
    (ClearChat, "clearChat", "Clear chat"),
    (CopyMessage, "copyMessage", "Copy message"),
    (CreateClip, "createClip", "Create clip"),
    (OpenChannelInBrowser, "openChannelInBrowser", "Open channel in browser"),
    (OpenChannelInStreamlink, "openChannelInStreamlink", "Open channel in Streamlink"),
    (OpenChannelInCustomPlayer, "openChannelInCustomPlayer", "Open channel in custom player"),
    (OpenModViewInBrowser, "openModViewInBrowser", "Open mod view in browser"),
    (OpenViewerList, "openViewerList", "Open viewer list"),
    (ScrollUp, "scrollUp", "Scroll up"),
    (ScrollDown, "scrollDown", "Scroll down"),
    (ScrollToTop, "scrollToTop", "Scroll to top"),
    (ScrollToBottom, "scrollToBottom", "Scroll to bottom"),
    (ReconnectActiveChannel, "reconnectActiveChannel", "Reconnect active channel"),
    (EnableHighlightSounds, "enableHighlightSounds", "Unmute channel notifications"),
    (DisableHighlightSounds, "disableHighlightSounds", "Mute channel notifications"),
    (ToggleHighlightSounds, "toggleHighlightSounds", "Toggle channel notifications"),
    (OpenSubscriptionPage, "openSubscriptionPage", "Open subscription page"),
    (FocusSplitLeft, "focusSplitLeft", "Focus split left"),
    (FocusSplitRight, "focusSplitRight", "Focus split right"),
    (FocusSplitUp, "focusSplitUp", "Focus split up"),
    (FocusSplitDown, "focusSplitDown", "Focus split down"),
);

const HOTKEYS_SEARCH: SettingDescriptor = SettingDescriptor {
    id: "hotkeys.search",
    label: "Search hotkeys",
    aliases: &["find", "jump", "filter"],
    control_id: "hotkeys-search",
    disclosures: &[SHORTCUTS_EDITOR_DISCLOSURE],
};

/// Hotkeys › Search.
pub const HOTKEYS_SEARCH_SETTINGS: &[SettingDescriptor] = hotkey_settings!(
    HOTKEYS_SEARCH;
    (SearchPreviousResult, "searchPreviousResult", "Previous search result"),
    (SearchNextResult, "searchNextResult", "Next search result"),
    (SearchOpenSelectedResult, "searchOpenSelectedResult", "Open selected search result"),
    (SearchPreviousPage, "searchPreviousPage", "Previous search page"),
    (SearchNextPage, "searchNextPage", "Next search page"),
    (SearchFirstResult, "searchFirstResult", "First search result"),
    (SearchLastResult, "searchLastResult", "Last search result"),
    (SearchClearQuery, "searchClearQuery", "Clear search query"),
);

const HOTKEYS_MODERATION: SettingDescriptor = SettingDescriptor {
    id: "hotkeys.moderation",
    label: "Moderation hotkeys",
    aliases: &["timeout", "ban", "delete", "mod"],
    control_id: "hotkeys-moderation",
    disclosures: &[SHORTCUTS_EDITOR_DISCLOSURE],
};

/// Hotkeys › Moderation.
pub const HOTKEYS_MODERATION_SETTINGS: &[SettingDescriptor] = hotkey_settings!(
    HOTKEYS_MODERATION;
    (ToggleModMode, "toggleModMode", "Toggle moderation mode"),
    (ToggleModLog, "toggleModLog", "Toggle moderation log"),
    (QuickTimeout30s, "quickTimeout30s", "Quick timeout 30 seconds"),
    (QuickTimeout10m, "quickTimeout10m", "Quick timeout 10 minutes"),
    (QuickBan, "quickBan", "Quick ban"),
);

const HOTKEYS_OTHER: SettingDescriptor = SettingDescriptor {
    id: "hotkeys.other",
    label: "Other hotkeys",
    aliases: &["window", "overlay", "misc"],
    control_id: "hotkeys-other",
    disclosures: &[SHORTCUTS_EDITOR_DISCLOSURE],
};

/// Hotkeys › Other.
pub const HOTKEYS_OTHER_SETTINGS: &[SettingDescriptor] = hotkey_settings!(
    HOTKEYS_OTHER;
    (ClearMentions, "clearMentions", "Clear mentions"),
    (MarkWhispersRead, "markWhispersRead", "Mark whispers read"),
    (CloseThreadViewer, "closeThreadViewer", "Close thread viewer"),
    (ReloadEmotes, "reloadEmotes", "Reload emotes"),
    (ToggleFullscreen, "toggleFullscreen", "Toggle fullscreen"),
    (OpenDebugPopup, "openDebugPopup", "Open debug popup"),
    (ToggleCheatSheet, "toggleCheatSheet", "Toggle cheat sheet"),
    (ToggleMetrics, "toggleMetrics", "Toggle metrics"),
    (ToggleStreamerOverlay, "toggleStreamerOverlay", "Toggle streamer overlay"),
    (ReopenClosedSplit, "reopenClosedSplit", "Reopen closed split"),
    (ReopenClosedTab, "reopenClosedTab", "Reopen closed tab"),
    (ToggleLocalR9K, "toggleLocalR9K", "Toggle local R9K"),
    (QuickSwitcher, "quickSwitcher", "Quick switcher"),
    (ToggleTransparency, "toggleTransparency", "Toggle transparency"),
    (ToggleAlwaysOnTop, "toggleAlwaysOnTop", "Toggle always on top"),
);

const HOTKEYS_CUSTOM: SettingDescriptor = SettingDescriptor {
    id: "hotkeys.custom",
    label: "Custom command hotkeys",
    aliases: &["custom command binding"],
    control_id: "custom-hotkey-label",
    disclosures: &[SHORTCUTS_EDITOR_DISCLOSURE],
};

const HOTKEYS_CUSTOM_COMMAND: SettingDescriptor = SettingDescriptor {
    id: "hotkeys.custom-command",
    label: "Custom hotkey command",
    aliases: &["custom hotkey payload", "hotkey variables"],
    control_id: "custom-hotkey-command",
    disclosures: &[SHORTCUTS_EDITOR_DISCLOSURE],
};

const HOTKEYS_RESET_ALL: SettingDescriptor = SettingDescriptor {
    id: "hotkeys.reset-all",
    label: "Reset all shortcuts",
    aliases: &["Reset all to defaults", "reset", "defaults", "restore"],
    control_id: "hotkey-reset-all",
    disclosures: &[SHORTCUTS_EDITOR_DISCLOSURE],
};

/// Hotkeys › Custom hotkeys. Reset All is a page action rather than its own
/// anchor, so it rides along with the last block.
pub const HOTKEYS_CUSTOM_SETTINGS: &[SettingDescriptor] =
    &[HOTKEYS_CUSTOM, HOTKEYS_CUSTOM_COMMAND, HOTKEYS_RESET_ALL];

#[derive(Clone, Copy, PartialEq, Eq)]
enum CustomHotkeyEditorField {
    Label,
    Command,
}

#[derive(Clone, Default)]
struct CustomHotkeyEditorState {
    label: String,
    command: String,
    editing_id: Option<String>,
    /// A not-yet-persisted custom action has a stable id only after its first
    /// capture. The binding remains a draft until Add saves the action.
    draft_id: Option<String>,
    draft_binding: Option<hotkeys::HotkeyBinding>,
    focused: Option<CustomHotkeyEditorField>,
    status: Option<String>,
}

static CUSTOM_HOTKEY_EDITOR: LazyLock<EditorState<CustomHotkeyEditorState>> =
    LazyLock::new(|| EditorState::new("custom hotkeys", CustomHotkeyEditorState::default()));

#[derive(Default)]
struct HotkeyEditorView {
    show_modified_only: bool,
    started_capture: Option<OwnedHotkeyCapture>,
}

struct HotkeyEditorInputs {
    filter: Entity<InputState>,
    label: Entity<InputState>,
    command: Entity<InputState>,
    _subscriptions: Vec<Subscription>,
}

struct ShortcutsView<'a> {
    query: &'a str,
    show_modified_only: bool,
    selected: Option<&'static str>,
    matched: &'a [&'static str],
    capturing: Option<&'a str>,
    conflicts: &'a HashSet<String>,
    conflict_labels: &'a HashMap<String, Vec<String>>,
    modified: &'a HashSet<HotkeyAction>,
    bindings: &'a HashMap<String, hotkeys::HotkeyBinding>,
    stacked: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CaptureOrigin {
    Row,
    Editor,
}

#[derive(Clone, Debug)]
struct OwnedHotkeyCapture {
    key: String,
    origin: CaptureOrigin,
}

thread_local! {
    static HOTKEY_EDITOR_INPUTS: RefCell<HashMap<EntityId, HotkeyEditorInputs>> = RefCell::new(HashMap::new());
    static HOTKEY_EDITOR_VIEWS: RefCell<HashMap<EntityId, HotkeyEditorView>> = RefCell::new(HashMap::new());
    static HOTKEY_CAPTURE_OWNERS: RefCell<HashMap<EntityId, OwnedHotkeyCapture>> = RefCell::new(HashMap::new());
    static CAPTURE_BLUR_SUBSCRIPTIONS: RefCell<HashMap<EntityId, HashMap<String, Subscription>>> = RefCell::new(HashMap::new());
}

/// The view also remembers the request, so host teardown remains safe even if
/// the active-owner record is lost while a conflict is waiting for retry.
fn capture_for_host(owner: EntityId) -> Option<OwnedHotkeyCapture> {
    HOTKEY_CAPTURE_OWNERS
        .try_with(|owners| owners.borrow().get(&owner).cloned())
        .ok()
        .flatten()
        .or_else(|| {
            HOTKEY_EDITOR_VIEWS
                .try_with(|views| {
                    views
                        .borrow()
                        .get(&owner)
                        .and_then(|view| view.started_capture.clone())
                })
                .ok()
                .flatten()
        })
}

fn active_settings_capture() -> Option<(EntityId, OwnedHotkeyCapture)> {
    let key = get_capturing_storage_key()?;
    HOTKEY_EDITOR_VIEWS
        .try_with(|views| {
            views.borrow().iter().find_map(|(owner, view)| {
                view.started_capture
                    .as_ref()
                    .filter(|capture| capture.key == key)
                    .map(|capture| (*owner, capture.clone()))
            })
        })
        .ok()
        .flatten()
}

fn clear_capture_records() {
    let _ = HOTKEY_CAPTURE_OWNERS.try_with(|owners| owners.borrow_mut().clear());
    let _ = HOTKEY_EDITOR_VIEWS.try_with(|views| {
        for view in views.borrow_mut().values_mut() {
            view.started_capture = None;
        }
    });
}

pub(crate) fn release_hotkey_editor_inputs_for_host(owner: EntityId) {
    cancel_settings_hotkey_capture(owner);
    let removed = HOTKEY_EDITOR_INPUTS
        .try_with(|entries| entries.borrow_mut().remove(&owner))
        .ok()
        .flatten();
    drop(removed);
    let removed = CAPTURE_BLUR_SUBSCRIPTIONS
        .try_with(|subscriptions| subscriptions.borrow_mut().remove(&owner))
        .ok()
        .flatten();
    drop(removed);
    let _ = HOTKEY_EDITOR_VIEWS.try_with(|views| {
        if let Some(view) = views.borrow_mut().get_mut(&owner) {
            view.started_capture = None;
        }
    });
}

pub(crate) fn forget_hotkey_editor_host(owner: EntityId) {
    release_hotkey_editor_inputs_for_host(owner);
    let _ = HOTKEY_EDITOR_VIEWS.try_with(|views| {
        views.borrow_mut().remove(&owner);
    });
}

fn start_capture(owner: EntityId, storage_key: String, origin: CaptureOrigin) {
    cancel_all_settings_hotkey_captures();
    let capture = OwnedHotkeyCapture {
        key: storage_key.clone(),
        origin,
    };
    HOTKEY_CAPTURE_OWNERS.with(|owners| {
        owners.borrow_mut().insert(owner, capture.clone());
    });
    HOTKEY_EDITOR_VIEWS.with(|views| {
        views.borrow_mut().entry(owner).or_default().started_capture = Some(capture);
    });
    hotkeys::start_hotkey_capture_for_key(storage_key);
}

fn start_settings_hotkey_capture(owner: EntityId, storage_key: String) {
    start_capture(owner, storage_key, CaptureOrigin::Row);
}

pub(crate) fn cancel_settings_hotkey_capture(owner: EntityId) {
    let started = capture_for_host(owner);
    if started.is_some() {
        clear_capture_rejection();
    }
    let key = get_capturing_storage_key();
    if started
        .as_ref()
        .is_some_and(|capture| key.as_deref() == Some(capture.key.as_str()))
    {
        hotkeys::cancel_capture();
    }
    let _ = HOTKEY_CAPTURE_OWNERS.try_with(|owners| {
        owners.borrow_mut().remove(&owner);
    });
    let _ = HOTKEY_EDITOR_VIEWS.try_with(|views| {
        if let Some(view) = views.borrow_mut().get_mut(&owner) {
            view.started_capture = None;
        }
    });
}

fn register_capture_blur<T: 'static>(
    id: &str,
    focus: &FocusHandle,
    key: Option<String>,
    origin: CaptureOrigin,
    window: &mut Window,
    cx: &mut Context<T>,
) {
    let owner = cx.entity_id();
    let present = CAPTURE_BLUR_SUBSCRIPTIONS.with(|subscriptions| {
        subscriptions
            .borrow()
            .get(&owner)
            .is_some_and(|subscriptions| subscriptions.contains_key(id))
    });
    if present {
        return;
    }
    let subscription = cx.on_blur(focus, window, move |_, _, cx| {
        if capture_for_host(owner).is_some_and(|capture| {
            capture.origin == origin && key.as_ref().is_none_or(|key| capture.key == *key)
        }) {
            cancel_settings_hotkey_capture(owner);
            cx.notify();
        }
    });
    CAPTURE_BLUR_SUBSCRIPTIONS.with(|subscriptions| {
        subscriptions
            .borrow_mut()
            .entry(owner)
            .or_default()
            .insert(id.to_string(), subscription)
    });
}

pub(crate) fn cancel_all_settings_hotkey_captures() {
    if active_settings_capture().is_some() {
        hotkeys::cancel_capture();
        clear_capture_rejection();
    }
    clear_capture_records();
}

fn toggle_settings_hotkey_capture<T: 'static>(
    storage_key: String,
    is_capturing: bool,
    window: &mut Window,
    focus: &FocusHandle,
    cx: &mut Context<T>,
) {
    clear_capture_rejection();
    if is_capturing {
        cancel_settings_hotkey_capture(cx.entity_id());
    } else {
        register_capture_blur(
            &format!("capture-row-{storage_key}"),
            focus,
            Some(storage_key.clone()),
            CaptureOrigin::Row,
            window,
            cx,
        );
        start_settings_hotkey_capture(cx.entity_id(), storage_key);
    }
    cx.notify();
}

pub(crate) fn settings_hotkey_capture_owned_by(owner: EntityId) -> bool {
    capture_for_host(owner)
        .is_some_and(|capture| get_capturing_storage_key().as_deref() == Some(capture.key.as_str()))
}

fn editor_capture_active(owner: EntityId) -> bool {
    capture_for_host(owner).is_some_and(|capture| {
        capture.origin == CaptureOrigin::Editor
            && get_capturing_storage_key().as_deref() == Some(capture.key.as_str())
    })
}

pub(crate) fn complete_settings_hotkey_capture(binding: hotkeys::HotkeyBinding) {
    let Some((_, request)) = active_settings_capture() else {
        return;
    };
    if request.origin == CaptureOrigin::Editor {
        if !hotkeys::validate_captured_binding(&binding) {
            return;
        }
        let _ = CUSTOM_HOTKEY_EDITOR.apply_user_edit(recovered_custom_hotkey_editor, |editor| {
            editor.draft_binding = Some(binding.clone());
            editor.status = Some(format!("Shortcut set to {}", binding.to_display()));
        });
        hotkeys::cancel_capture();
    } else {
        hotkeys::complete_capture(binding);
    }
    // A conflict intentionally keeps capture active. Keep its owner and origin
    // until retry succeeds or an explicit cancel/teardown ends the request.
    if !hotkeys::is_capturing() {
        clear_capture_records();
    }
}

/// Render the unified shortcut editor. Historical category anchors are aliases
/// of this one visible block; their descriptors remain attached to the rows.
pub fn render_hotkeys_section<T: 'static>(
    ui: &mut SettingsPageUi,
    window: &mut Window,
    cx: &mut Context<T>,
) -> SettingsPage {
    let disclosure = ui.disclosure(SHORTCUTS_EDITOR_DISCLOSURE, cx);
    let is_open = disclosure.is_expanded();
    let capturing = settings_hotkey_capture_owned_by(cx.entity_id())
        .then(get_capturing_storage_key)
        .flatten();
    let hotkey_snapshot = hotkeys::editor_snapshot(is_open);
    let bindings = hotkey_snapshot.bindings;
    let custom_actions = hotkey_snapshot.custom_actions;
    let conflicts = hotkey_snapshot.conflicts;
    let conflict_labels = hotkey_snapshot.conflict_labels;
    let capture_error = CAPTURE_REJECTION
        .lock_or_recover()
        .clone()
        .or_else(hotkeys::get_capture_error);
    let custom_editor = custom_hotkey_editor_snapshot_from(&custom_actions);
    if is_open {
        ensure_hotkey_editor_inputs(&custom_editor.draft, window, cx);
    } else {
        release_hotkey_editor_inputs_for_host(cx.entity_id());
        if custom_editor.draft.focused.is_some() {
            CUSTOM_HOTKEY_EDITOR.sync_and_snapshot(recovered_custom_hotkey_editor, |editor| {
                editor.focused = None;
                false
            });
        }
    }
    let selected = ui.selected_setting;
    let matched_settings = ui.matched_settings.clone();
    let query = hotkey_filter_input(cx)
        .map(|input| input.read(cx).value().to_string())
        .unwrap_or_default();
    let show_modified_only = hotkey_editor_view(cx).show_modified_only;
    let builtin_actions = all_builtin_actions();
    let modified_actions: HashSet<_> = builtin_actions
        .iter()
        .filter(|action| hotkey_is_modified(**action, &bindings))
        .copied()
        .collect();
    let modified = modified_actions.len();
    let custom_bindings = custom_actions
        .iter()
        .filter(|action| snapshot_binding(&bindings, &action.storage_key()).is_some())
        .count();
    let mut summary = format!("{} actions", builtin_actions.len());
    if modified > 0 {
        summary.push_str(&format!(" · {modified} modified"));
    }
    if custom_bindings > 0 {
        let label = if custom_bindings == 1 {
            "custom binding"
        } else {
            "custom bindings"
        };
        summary.push_str(&format!(" · {custom_bindings} {label}"));
    }
    if custom_editor.needs_review {
        summary.push_str(" · Needs review");
    } else if custom_editor.draft.editing_id.is_some() {
        summary.push_str(" · Editing…");
    } else if custom_hotkey_editor_has_draft(&custom_editor.draft) {
        summary.push_str(" · Unsaved draft");
    }

    SettingsPage::new(SettingsSectionId::Hotkeys)
        .quiet()
        .subsection(
            SettingsSubsectionId::HotkeysNavigation,
            settings_group(
                "shortcuts-editor-group",
                "Keyboard shortcuts",
                false,
                settings_group_contents(
                    "shortcuts-editor-content",
                    "Keyboard shortcuts",
                    false,
                    div()
                        .w_full()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .when_some(capture_error, |content, message| {
                            content.child(capture_error_notice(message))
                        })
                        .child(setting_target(
                            HOTKEYS_EDITOR_SETTINGS[0],
                            settings_summary_disclosure(
                                SHORTCUTS_EDITOR_DISCLOSURE,
                                "Keyboard shortcuts",
                                &summary,
                                &disclosure,
                                cx,
                                move |cx| {
                                    render_shortcuts_editor_body(
                                        ShortcutsView {
                                            query: &query,
                                            show_modified_only,
                                            selected,
                                            matched: &matched_settings,
                                            capturing: capturing.as_deref(),
                                            conflicts: &conflicts,
                                            conflict_labels: &conflict_labels,
                                            modified: &modified_actions,
                                            bindings: &bindings,
                                            stacked: ui.stacked,
                                        },
                                        custom_editor,
                                        custom_actions,
                                        cx,
                                    )
                                },
                            ),
                        )),
                ),
            ),
        )
}

fn capture_error_notice(message: String) -> impl IntoElement {
    div()
        .w_full()
        .px_3()
        .py_2()
        .rounded(Radii::md())
        .bg(colors::warning().opacity(0.12))
        .border_1()
        .border_color(colors::warning())
        .text_sm()
        .font_weight(FontWeight::MEDIUM)
        .text_color(colors::warning())
        .child(message)
}

fn render_shortcuts_editor_body<T: 'static>(
    view: ShortcutsView<'_>,
    custom_editor: EditorSnapshot<CustomHotkeyEditorState>,
    custom_actions: Vec<CustomHotkeyAction>,
    cx: &mut Context<T>,
) -> impl IntoElement + use<T> {
    div()
        .w_full()
        .min_w_0()
        .flex()
        .flex_col()
        .gap_3()
        .child(render_shortcut_filter_row(view.show_modified_only, cx))
        .child(render_category::<T>(
            "Navigation",
            NAVIGATION_ACTIONS,
            HOTKEYS_NAVIGATION,
            &view,
            cx,
        ))
        .child(render_category::<T>(
            "Chat",
            CHAT_ACTIONS,
            HOTKEYS_CHAT,
            &view,
            cx,
        ))
        .child(render_category::<T>(
            "Search",
            SEARCH_ACTIONS,
            HOTKEYS_SEARCH,
            &view,
            cx,
        ))
        .child(render_category::<T>(
            "Moderation",
            MODERATION_ACTIONS,
            HOTKEYS_MODERATION,
            &view,
            cx,
        ))
        .child(render_category::<T>(
            "Other",
            OTHER_ACTIONS,
            HOTKEYS_OTHER,
            &view,
            cx,
        ))
        .child(settings_subheading("Custom bindings"))
        .child(render_custom_hotkeys_section(
            custom_editor,
            &custom_actions,
            &view,
            cx,
        ))
        .child(setting_target(
            HOTKEYS_RESET_ALL,
            render_reset_all_button::<T>(cx),
        ))
}

fn render_shortcut_filter_row<T: 'static>(
    show_modified_only: bool,
    cx: &mut Context<T>,
) -> impl IntoElement + use<T> {
    div()
        .w_full()
        .min_w_0()
        .flex()
        .flex_wrap()
        .items_center()
        .gap_2()
        .child(
            div()
                .flex_1()
                .min_w(px(220.0))
                .when_some(hotkey_filter_input(cx), |row, input| {
                    row.child(Input::new(&input).accessibility_label("Filter keyboard shortcuts"))
                }),
        )
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .flex_shrink_0()
                .child(
                    div()
                        .text_xs()
                        .text_color(colors::text_secondary())
                        .child("Modified only"),
                )
                .child(labelled_toggle(
                    "hotkey-show-modified-only",
                    "Modified only",
                    show_modified_only,
                    cx,
                    move |enabled, cx| {
                        update_hotkey_editor_view(cx, |view| view.show_modified_only = enabled);
                        cx.notify();
                    },
                )),
        )
}

/// Categories are quiet subheadings inside the one expanded editor, not
/// separate disclosure surfaces.
fn render_category<T: 'static>(
    heading: &str,
    actions: &[HotkeyAction],
    category: SettingDescriptor,
    view: &ShortcutsView<'_>,
    cx: &mut Context<T>,
) -> impl IntoElement + use<T> {
    let rows: Vec<_> = actions
        .iter()
        .copied()
        .filter(|action| {
            let descriptor = builtin_hotkey_descriptor(*action, category);
            builtin_action_is_visible(
                *action,
                heading,
                view.query,
                view.show_modified_only,
                view.selected == Some(category.id)
                    || view.selected == Some(descriptor.id)
                    || view.matched.contains(&category.id)
                    || view.matched.contains(&descriptor.id),
                view.bindings,
                view.modified.contains(action),
            )
        })
        .collect();
    setting_target(
        category,
        div()
            .w_full()
            .min_w_0()
            .flex()
            .flex_col()
            .gap_0()
            .when(!rows.is_empty(), |category_el| {
                category_el.child(settings_subheading(heading)).children(
                    rows.into_iter()
                        .map(|action| render_hotkey_row::<T>(action, category, view, cx)),
                )
            }),
    )
}

fn builtin_action_is_visible(
    action: HotkeyAction,
    category: &str,
    query: &str,
    show_modified_only: bool,
    selected: bool,
    bindings: &HashMap<String, hotkeys::HotkeyBinding>,
    modified: bool,
) -> bool {
    selected
        || (!show_modified_only || modified)
            && builtin_action_matches_query(action, category, query, bindings)
}

fn all_builtin_actions() -> Vec<HotkeyAction> {
    [
        NAVIGATION_ACTIONS,
        CHAT_ACTIONS,
        SEARCH_ACTIONS,
        MODERATION_ACTIONS,
        OTHER_ACTIONS,
    ]
    .into_iter()
    .flatten()
    .copied()
    .collect()
}

fn hotkey_is_modified(
    action: HotkeyAction,
    bindings: &HashMap<String, hotkeys::HotkeyBinding>,
) -> bool {
    let current = snapshot_binding(bindings, &action.to_string()).unwrap_or_default();
    let default = HOTKEY_DEFAULTS
        .get(&action.to_string())
        .cloned()
        .unwrap_or_default();
    current != default
}

fn builtin_action_matches_query(
    action: HotkeyAction,
    category: &str,
    query: &str,
    bindings: &HashMap<String, hotkeys::HotkeyBinding>,
) -> bool {
    let query = query.trim().to_lowercase();
    if query.is_empty() {
        return true;
    }
    let binding = snapshot_binding(bindings, &action.to_string())
        .map(|binding| binding.to_display())
        .unwrap_or_else(|| "Not set".to_string());
    [
        action.display_name(),
        action.scope_label(),
        action.description(),
        category,
        binding.as_str(),
    ]
    .iter()
    .any(|value| value.to_lowercase().contains(&query))
}

fn custom_hotkey_editor_has_draft(editor: &CustomHotkeyEditorState) -> bool {
    !editor.label.trim().is_empty()
        || !editor.command.trim().is_empty()
        || editor.draft_id.is_some()
        || editor.draft_binding.is_some()
}

fn hotkey_editor_view<T: 'static>(cx: &Context<T>) -> HotkeyEditorView {
    HOTKEY_EDITOR_VIEWS.with(|views| {
        views
            .borrow()
            .get(&cx.entity_id())
            .map(|view| HotkeyEditorView {
                show_modified_only: view.show_modified_only,
                started_capture: view.started_capture.clone(),
            })
            .unwrap_or_default()
    })
}

fn update_hotkey_editor_view<T: 'static>(
    cx: &Context<T>,
    update: impl FnOnce(&mut HotkeyEditorView),
) {
    HOTKEY_EDITOR_VIEWS.with(|views| {
        update(views.borrow_mut().entry(cx.entity_id()).or_default());
    });
}

fn hotkey_filter_input<T: 'static>(cx: &Context<T>) -> Option<Entity<InputState>> {
    HOTKEY_EDITOR_INPUTS.with(|inputs| {
        inputs
            .borrow()
            .get(&cx.entity_id())
            .map(|inputs| inputs.filter.clone())
    })
}

fn hotkey_custom_input<T: 'static>(
    field: CustomHotkeyEditorField,
    cx: &Context<T>,
) -> Option<Entity<InputState>> {
    HOTKEY_EDITOR_INPUTS.with(|inputs| {
        inputs
            .borrow()
            .get(&cx.entity_id())
            .map(|inputs| match field {
                CustomHotkeyEditorField::Label => inputs.label.clone(),
                CustomHotkeyEditorField::Command => inputs.command.clone(),
            })
    })
}

fn ensure_hotkey_editor_inputs<T: 'static>(
    editor: &CustomHotkeyEditorState,
    window: &mut Window,
    cx: &mut Context<T>,
) {
    let owner = cx.entity_id();
    let present = HOTKEY_EDITOR_INPUTS.with(|inputs| inputs.borrow().contains_key(&owner));
    if !present {
        let filter = cx.new(|cx| InputState::new(window, cx).with_placeholder("Filter actions"));
        let label = cx.new(|cx| {
            InputState::new(window, cx)
                .with_placeholder("Label, for example Reply last whisper")
                .with_value(editor.label.clone())
        });
        let command = cx.new(|cx| {
            InputState::new(window, cx)
                .with_placeholder("Command or text, for example /r Thanks")
                .with_value(editor.command.clone())
        });
        let filter_subscription = cx.subscribe_in(
            &filter,
            window,
            move |_this, _input, event: &InputEvent, _window, cx| {
                if matches!(event, InputEvent::Focus)
                    && settings_hotkey_capture_owned_by(cx.entity_id())
                {
                    cancel_settings_hotkey_capture(cx.entity_id());
                    cx.notify();
                }
                if matches!(event, InputEvent::Change) {
                    cx.notify();
                }
            },
        );
        let label_subscription =
            subscribe_custom_hotkey_input(&label, CustomHotkeyEditorField::Label, window, cx);
        let command_subscription =
            subscribe_custom_hotkey_input(&command, CustomHotkeyEditorField::Command, window, cx);
        HOTKEY_EDITOR_INPUTS.with(|inputs| {
            inputs.borrow_mut().insert(
                owner,
                HotkeyEditorInputs {
                    filter,
                    label,
                    command,
                    _subscriptions: vec![
                        filter_subscription,
                        label_subscription,
                        command_subscription,
                    ],
                },
            );
        });
    }
    sync_hotkey_custom_inputs(editor, window, cx);
}

fn subscribe_custom_hotkey_input<T: 'static>(
    input: &Entity<InputState>,
    field: CustomHotkeyEditorField,
    window: &mut Window,
    cx: &mut Context<T>,
) -> Subscription {
    cx.subscribe_in(
        input,
        window,
        move |_this, input, event: &InputEvent, _window, cx| {
            match event {
                InputEvent::Focus => {
                    if settings_hotkey_capture_owned_by(cx.entity_id()) {
                        cancel_settings_hotkey_capture(cx.entity_id());
                    }
                    CUSTOM_HOTKEY_EDITOR.sync_and_snapshot(
                        recovered_custom_hotkey_editor,
                        |editor| {
                            if editor.focused == Some(field) {
                                return false;
                            }
                            editor.focused = Some(field);
                            false
                        },
                    );
                }
                InputEvent::Change => {
                    let value = input.read(cx).value().to_string();
                    let snapshot = custom_hotkey_editor_snapshot();
                    if !custom_hotkey_text_changed(&snapshot.draft, field, &value) {
                        return;
                    }
                    update_custom_hotkey_editor(|editor| {
                        match field {
                            CustomHotkeyEditorField::Label => editor.label = value,
                            CustomHotkeyEditorField::Command => editor.command = value,
                        }
                        editor.status = None;
                    });
                }
                InputEvent::Blur => {
                    CUSTOM_HOTKEY_EDITOR.sync_and_snapshot(
                        recovered_custom_hotkey_editor,
                        |editor| {
                            if editor.focused != Some(field) {
                                return false;
                            }
                            editor.focused = None;
                            false
                        },
                    );
                }
                InputEvent::PressEnter => {}
            }
            cx.notify();
        },
    )
}

fn sync_hotkey_custom_inputs<T: 'static>(
    editor: &CustomHotkeyEditorState,
    window: &mut Window,
    cx: &mut Context<T>,
) {
    for (field, value) in [
        (CustomHotkeyEditorField::Label, editor.label.as_str()),
        (CustomHotkeyEditorField::Command, editor.command.as_str()),
    ] {
        let Some(input) = hotkey_custom_input(field, cx) else {
            continue;
        };
        let value = normalize_single_line_text(value);
        if input.read(cx).value() != value {
            input.update(cx, |input, cx| {
                input.set_value(value.to_string(), window, cx)
            });
        }
    }
}

fn custom_hotkey_text_changed(
    editor: &CustomHotkeyEditorState,
    field: CustomHotkeyEditorField,
    value: &str,
) -> bool {
    let authority = match field {
        CustomHotkeyEditorField::Label => &editor.label,
        CustomHotkeyEditorField::Command => &editor.command,
    };
    normalize_single_line_text(authority) != value
}

fn builtin_hotkey_descriptor(
    action: HotkeyAction,
    category: SettingDescriptor,
) -> SettingDescriptor {
    static INDEX: LazyLock<HashMap<HotkeyAction, SettingDescriptor>> = LazyLock::new(|| {
        [
            (NAVIGATION_ACTIONS, HOTKEYS_NAVIGATION_SETTINGS),
            (CHAT_ACTIONS, HOTKEYS_CHAT_SETTINGS),
            (SEARCH_ACTIONS, HOTKEYS_SEARCH_SETTINGS),
            (MODERATION_ACTIONS, HOTKEYS_MODERATION_SETTINGS),
            (OTHER_ACTIONS, HOTKEYS_OTHER_SETTINGS),
        ]
        .into_iter()
        .flat_map(|(actions, descriptors)| {
            actions
                .iter()
                .copied()
                .zip(descriptors.iter().skip(1).copied())
        })
        .collect()
    });
    INDEX.get(&action).copied().unwrap_or(category)
}

/// Render a single hotkey row with rebind support
fn render_hotkey_row<T: 'static>(
    action: HotkeyAction,
    category: SettingDescriptor,
    view: &ShortcutsView<'_>,
    cx: &mut Context<T>,
) -> impl IntoElement + use<T> {
    let ShortcutsView {
        capturing,
        conflicts,
        conflict_labels,
        modified,
        bindings,
        stacked,
        ..
    } = *view;
    let storage_key = action.to_string();
    let is_capturing =
        settings_hotkey_capture_owned_by(cx.entity_id()) && capturing == Some(storage_key.as_str());
    let typing_detail = typing_binding_detail(bindings, &storage_key);
    let is_typing_key = typing_detail.is_some();
    let is_conflicting = conflicts.contains(&storage_key) || is_typing_key;
    let binding = snapshot_binding(bindings, &storage_key);
    let has_binding = binding.is_some();
    let is_modified = modified.contains(&action);
    let binding_display = binding
        .as_ref()
        .map(|b| b.to_display())
        .unwrap_or_else(|| "Not set".to_string());

    let descriptor = builtin_hotkey_descriptor(action, category);
    let entity = cx.entity();
    let a11y_entity = entity.clone();
    let capture_storage_key = storage_key.clone();
    let accessible_capture_storage_key = storage_key.clone();
    let capture_id = format!("hotkey-bind-{action:?}");
    let capture_focus = control_focus(&capture_id, cx);
    let activation_focus = capture_focus.clone();
    let accessible_focus = capture_focus.clone();
    let conflict_detail = if is_conflicting {
        let names = conflict_labels
            .get(&storage_key)
            .cloned()
            .unwrap_or_default();
        if names.is_empty() {
            None
        } else {
            Some(format!("Conflicts with {}", names.join(", ")))
        }
    } else {
        None
    }
    .or(typing_detail);

    let row = div()
        .id(SharedString::from(format!("hotkey-{:?}", action)))
        .w_full()
        .flex()
        .flex_wrap()
        .items_center()
        .min_h(px(40.0))
        .px_3()
        .py_1()
        .bg(if is_capturing {
            colors::accent().opacity(0.15)
        } else if is_conflicting {
            colors::warning().opacity(0.12)
        } else {
            transparent_black()
        })
        .border_b_1()
        .border_color(if is_capturing {
            colors::accent()
        } else if is_conflicting {
            colors::warning()
        } else {
            colors::border()
        })
        .child(
            div()
                .flex_1()
                .min_w(px(if stacked { 220.0 } else { 260.0 }))
                .flex()
                .flex_col()
                .gap_1()
                .child(
                    div()
                        .text_sm()
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(colors::text_primary())
                        .child(descriptor.label),
                )
                .when_some(conflict_detail, |row, detail| {
                    row.child(
                        div()
                            .text_xs()
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(colors::warning())
                            .child(detail),
                    )
                }),
        )
        .child(
            div()
                .flex()
                .flex_wrap()
                .items_center()
                .gap_2()
                .when(stacked, |controls| controls.w_full().justify_between())
                .when(is_conflicting, |s| {
                    s.child(
                        div()
                            .px_2()
                            .py_1()
                            .rounded(Radii::sm())
                            .bg(colors::warning().opacity(0.16))
                            .text_xs()
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(colors::warning())
                            .child(if is_typing_key {
                                if binding
                                    .as_ref()
                                    .is_some_and(|binding| binding.key.eq_ignore_ascii_case("tab"))
                                {
                                    "Reserved"
                                } else {
                                    "Types text"
                                }
                            } else {
                                "Conflict"
                            }),
                    )
                })
                .child(
                    div()
                        .id(SharedString::from(capture_id))
                        .min_w(px(160.0))
                        .px_3()
                        .py_1()
                        .rounded(Radii::sm())
                        .bg(if is_capturing {
                            colors::accent()
                        } else {
                            colors::bg_hover()
                        })
                        .border_1()
                        .border_color(if is_capturing {
                            colors::accent()
                        } else if is_conflicting {
                            colors::warning()
                        } else {
                            colors::border()
                        })
                        .role(Role::Button)
                        .map(|control| {
                            crate::components::settings::widgets::settings_keyboard_control_with_focus(
                                control, cx,
                             capture_focus.clone(), )
                        })
                        .a11y_synthetic_children(
                            crate::components::ui::accessibility::apply_description,
                        )
                        .aria_label(format!("Change {} shortcut", descriptor.label))
                        .cursor_pointer()
                        .hover(|s| {
                            if !is_capturing {
                                s.bg(colors::bg_elevated())
                            } else {
                                s
                            }
                        })
                        .on_activate(move |_, window, app| {
                            activation_focus.focus(window, app);
                            entity.update(app, |_, cx| {
                                toggle_settings_hotkey_capture(
                                    capture_storage_key.clone(),
                                    is_capturing,
                                    window,
                                    &activation_focus,
                                    cx,
                                );
                            });
                        })
                        .on_a11y_action(AccessibleAction::Click, move |_, window, app| {
                            accessible_focus.focus(window, app);
                            a11y_entity.update(app, |_, cx| {
                                toggle_settings_hotkey_capture(
                                    accessible_capture_storage_key.clone(),
                                    is_capturing,
                                    window,
                                    &accessible_focus,
                                    cx,
                                );
                            });
                        })
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(
                            div()
                                .text_sm()
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(if is_capturing {
                                    colors::text_on_accent_contrast()
                                } else {
                                    colors::text_primary()
                                })
                                .child(if is_capturing {
                                    "Press key…".to_string()
                                } else {
                                    binding_display
                                }),
                        ),
                )
                .child(
                    div()
                        .w(px(48.0))
                        .flex()
                        .justify_end()
                        .when(has_binding, |slot| {
                            slot.child(quiet_action_button(
                                format!("hotkey-clear-{action:?}"),
                                "Clear",
                                false,
                                cx,
                                move |cx| {
                                    hotkeys::clear_binding(action);
                                    cx.notify();
                                },
                            ))
                        }),
                )
                .child(
                    div()
                        .w(px(50.0))
                        .flex()
                        .justify_end()
                        .when(is_modified, |slot| {
                            slot.child(setting_reset(
                                format!("hotkey-reset-{action:?}"),
                                action.display_name(),
                                cx,
                                move |cx| {
                                    hotkeys::reset_binding(action);
                                    cx.notify();
                                },
                            ))
                        }),
                ),
        );
    if descriptor.id == category.id {
        row.into_any_element()
    } else {
        setting_target(descriptor, row).into_any_element()
    }
}

fn render_custom_hotkeys_section<T: 'static>(
    editor: EditorSnapshot<CustomHotkeyEditorState>,
    actions: &[CustomHotkeyAction],
    view: &ShortcutsView<'_>,
    cx: &mut Context<T>,
) -> impl IntoElement + use<T> {
    let ShortcutsView {
        capturing,
        conflicts,
        conflict_labels,
        bindings,
        stacked,
        ..
    } = *view;
    let editor_generation = editor.generation;
    let editor_needs_review = editor.needs_review;
    let editor = editor.draft;
    let editing = editor.editing_id.is_some();

    div()
        .w_full()
        .flex()
        .flex_col()
        .gap_3()
        .child(div().max_w(px(640.0)).text_xs().text_color(colors::text_secondary()).child("Bind a command or chat payload to the active split. Variables: {input.text}, {channel}, {user}, {time}"))
        .child(render_custom_hotkey_editor(
            editor,
            editor_generation,
            editor_needs_review,
            actions,
            bindings,
            stacked,
            cx,
        ))
        .child(
            div()
                .flex()
                .flex_col()
                .gap_0()
                .when(actions.is_empty(), |el| {
                    el.child(empty_state("No custom command hotkeys configured."))
                })
                .children(
                    actions
                        .iter()
                        .cloned()
                        .map(|action| {
                            render_custom_hotkey_row(
                                action,
                                capturing,
                                conflicts,
                                conflict_labels,
                                bindings,
                                stacked,
                                cx,
                            )
                        }),
                ),
        )
        .when(editing, |section| {
            section.child(
                div()
                    .text_xs()
                    .text_color(colors::text_muted())
                    .child("Editing keeps the current binding unless you change it below."),
            )
        })
}

fn render_custom_hotkey_editor<T: 'static>(
    editor: CustomHotkeyEditorState,
    editor_generation: u64,
    editor_needs_review: bool,
    actions: &[CustomHotkeyAction],
    bindings: &HashMap<String, hotkeys::HotkeyBinding>,
    stacked: bool,
    cx: &mut Context<T>,
) -> impl IntoElement + use<T> {
    let editing = editor.editing_id.is_some();
    let has_draft = custom_hotkey_editor_has_draft(&editor);

    div()
        .flex()
        .flex_col()
        .gap_2()
        .child(setting_target(
            HOTKEYS_CUSTOM,
            render_editor_text_input(CustomHotkeyEditorField::Label, HOTKEYS_CUSTOM.label, cx),
        ))
        .child(setting_target(
            HOTKEYS_CUSTOM_COMMAND,
            render_editor_text_input(
                CustomHotkeyEditorField::Command,
                HOTKEYS_CUSTOM_COMMAND.label,
                cx,
            ),
        ))
        .child(render_custom_hotkey_capture(
            editor.clone(),
            actions,
            bindings,
            cx,
        ))
        .child(
            div()
                .flex()
                .flex_wrap()
                .items_center()
                .gap_2()
                .child(render_editor_button(
                    if editing { "Save" } else { "Add" },
                    true,
                    move |cx| {
                        if settings_hotkey_capture_owned_by(cx.entity_id()) {
                            cancel_settings_hotkey_capture(cx.entity_id());
                        }
                        save_custom_hotkey_editor(editor_generation);
                        cx.notify();
                    },
                    cx,
                ))
                .when(editing || has_draft, |row| {
                    row.child(render_editor_button(
                        if editing { "Cancel" } else { "Discard" },
                        false,
                        |cx| {
                            if settings_hotkey_capture_owned_by(cx.entity_id()) {
                                cancel_settings_hotkey_capture(cx.entity_id());
                            }
                            reset_custom_hotkey_editor();
                            cx.notify();
                        },
                        cx,
                    ))
                })
                .when_some(editor.status, |row, status| {
                    row.child(
                        div()
                            .text_xs()
                            .text_color(if editor_needs_review {
                                colors::error()
                            } else {
                                colors::text_muted()
                            })
                            .child(status),
                    )
                })
                .when(stacked, |row| row.w_full()),
        )
}

fn render_custom_hotkey_capture<T: 'static>(
    editor: CustomHotkeyEditorState,
    actions: &[CustomHotkeyAction],
    bindings: &HashMap<String, hotkeys::HotkeyBinding>,
    cx: &mut Context<T>,
) -> impl IntoElement + use<T> {
    let is_capturing = editor_capture_active(cx.entity_id());
    let saved_binding = || {
        editor
            .editing_id
            .as_deref()
            .and_then(|id| actions.iter().find(|action| action.id == id))
            .and_then(|action| snapshot_binding(bindings, &action.storage_key()))
    };
    let (display, has_binding) = if let Some(binding) = editor.draft_binding.as_ref() {
        let has_binding = !binding.key.trim().is_empty();
        (
            if has_binding {
                binding.to_display()
            } else {
                "Not set".to_string()
            },
            has_binding,
        )
    } else {
        let binding = saved_binding();
        (
            binding
                .as_ref()
                .map(hotkeys::HotkeyBinding::to_display)
                .unwrap_or_else(|| "Not set".to_string()),
            binding.is_some(),
        )
    };
    let entity = cx.entity();
    let accessible_entity = entity.clone();
    let capture_focus = control_focus("custom-hotkey-editor-bind", cx);
    let activation_focus = capture_focus.clone();
    let accessible_focus = capture_focus.clone();
    div()
        .w_full()
        .min_w_0()
        .flex()
        .flex_wrap()
        .items_center()
        .gap_2()
        .child(
            div()
                .text_xs()
                .text_color(colors::text_secondary())
                .child("Shortcut"),
        )
        .child(
            div()
                .id("custom-hotkey-editor-bind")
                .role(Role::Button)
                .map(|control| {
                    crate::components::settings::widgets::settings_keyboard_control_with_focus(
                        control,
                        cx,
                        capture_focus.clone(),
                    )
                })
                .a11y_synthetic_children(crate::components::ui::accessibility::apply_description)
                .aria_label("Capture custom shortcut")
                .min_w(px(160.0))
                .px_3()
                .py_1()
                .rounded(Radii::sm())
                .border_1()
                .border_color(if is_capturing {
                    colors::accent()
                } else {
                    colors::border()
                })
                .bg(if is_capturing {
                    colors::accent()
                } else {
                    colors::bg_hover()
                })
                .cursor_pointer()
                .on_activate(move |_, window, app| {
                    activation_focus.focus(window, app);
                    entity.update(app, |_, cx| {
                        begin_custom_hotkey_editor_capture(window, &activation_focus, cx)
                    });
                })
                .on_a11y_action(AccessibleAction::Click, move |_, window, app| {
                    accessible_focus.focus(window, app);
                    accessible_entity.update(app, |_, cx| {
                        begin_custom_hotkey_editor_capture(window, &accessible_focus, cx)
                    });
                })
                .child(
                    div()
                        .text_xs()
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(if is_capturing {
                            colors::text_on_accent_contrast()
                        } else {
                            colors::text_primary()
                        })
                        .child(if is_capturing {
                            "Press key…".to_string()
                        } else {
                            display
                        }),
                ),
        )
        .when(has_binding, |row| {
            row.child(quiet_action_button(
                "custom-hotkey-editor-clear",
                "Clear",
                false,
                cx,
                |cx| {
                    if settings_hotkey_capture_owned_by(cx.entity_id()) {
                        cancel_settings_hotkey_capture(cx.entity_id());
                    }
                    update_custom_hotkey_editor(|editor| {
                        editor.draft_binding = Some(hotkeys::HotkeyBinding::default());
                    });
                    cx.notify();
                },
            ))
        })
}

fn begin_custom_hotkey_editor_capture<T: 'static>(
    window: &mut Window,
    focus: &FocusHandle,
    cx: &mut Context<T>,
) {
    clear_capture_rejection();
    let snapshot = custom_hotkey_editor_snapshot();
    let Some(action) = custom_hotkey_editor_capture_target(&snapshot.draft) else {
        update_custom_hotkey_editor(|editor| {
            editor.status = Some("Enter a label and command before capturing".to_string());
        });
        cx.notify();
        return;
    };
    if editor_capture_active(cx.entity_id()) {
        cancel_settings_hotkey_capture(cx.entity_id());
    } else {
        let key = action.storage_key();
        register_capture_blur(
            "capture-editor",
            focus,
            None,
            CaptureOrigin::Editor,
            window,
            cx,
        );
        start_capture(cx.entity_id(), key.clone(), CaptureOrigin::Editor);
        update_custom_hotkey_editor(|editor| {
            if editor.editing_id.is_none() {
                editor.draft_id = Some(action.id);
            }
        });
    }
    cx.notify();
}

fn custom_hotkey_editor_capture_target(
    editor: &CustomHotkeyEditorState,
) -> Option<CustomHotkeyAction> {
    if let Some(editing_id) = editor.editing_id.as_deref() {
        return custom_hotkeys()
            .into_iter()
            .find(|action| action.id == editing_id);
    }
    let name = editor.label.trim();
    let command = editor.command.trim();
    if name.is_empty() || command.is_empty() {
        return None;
    }
    Some(CustomHotkeyAction {
        id: editor
            .editing_id
            .clone()
            .or_else(|| editor.draft_id.clone())
            .unwrap_or_else(|| Uuid::new_v4().to_string()),
        name: name.to_string(),
        command: command.to_string(),
    })
}

fn render_custom_hotkey_row<T: 'static>(
    action: CustomHotkeyAction,
    capturing: Option<&str>,
    conflicts: &HashSet<String>,
    conflict_labels: &HashMap<String, Vec<String>>,
    bindings: &HashMap<String, hotkeys::HotkeyBinding>,
    stacked: bool,
    cx: &mut Context<T>,
) -> impl IntoElement + use<T> {
    let storage_key = action.storage_key();
    let clear_id = SharedString::from(format!("custom-hotkey-clear-{storage_key}"));
    let edit_id = SharedString::from(format!("custom-hotkey-edit-{storage_key}"));
    let is_capturing =
        settings_hotkey_capture_owned_by(cx.entity_id()) && capturing == Some(storage_key.as_str());
    let typing_detail = typing_binding_detail(bindings, &storage_key);
    let is_typing_key = typing_detail.is_some();
    let is_conflicting = conflicts.contains(&storage_key) || is_typing_key;
    let binding = snapshot_binding(bindings, &storage_key);
    let has_binding = binding.is_some();
    let binding_display = binding
        .map(|binding| binding.to_display())
        .unwrap_or_else(|| "Not set".to_string());
    let conflict_detail = if is_conflicting {
        let names = conflict_labels
            .get(&storage_key)
            .cloned()
            .unwrap_or_default();
        if names.is_empty() {
            None
        } else {
            Some(format!("Conflicts with {}", names.join(", ")))
        }
    } else {
        None
    }
    .or(typing_detail);
    let entity = cx.entity();
    let a11y_entity = entity.clone();
    let capture_storage_key = storage_key.clone();
    let accessible_capture_storage_key = storage_key.clone();
    let capture_id = format!("custom-hotkey-bind-{}", action.id);
    let capture_focus = control_focus(&capture_id, cx);
    let activation_focus = capture_focus.clone();
    let accessible_focus = capture_focus.clone();

    div()
        .id(SharedString::from(format!("custom-hotkey-{}", action.id)))
        .w_full()
        .flex()
        .flex_wrap()
        .items_center()
        .justify_between()
        .min_h(px(40.0))
        .px_3()
        .py_1()
        .bg(if is_capturing {
            colors::accent().opacity(0.15)
        } else if is_conflicting {
            colors::warning().opacity(0.12)
        } else {
            transparent_black()
        })
        .border_b_1()
        .border_color(if is_capturing {
            colors::accent()
        } else if is_conflicting {
            colors::warning()
        } else {
            colors::border()
        })
        .child(
            div()
                .flex_1()
                .min_w(px(if stacked { 220.0 } else { 260.0 }))
                .flex()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .text_sm()
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(colors::text_primary())
                        .child(action.name.clone()),
                )
                .child(
                    div().min_w_0().flex_1().flex().items_center().child(
                        div()
                            .min_w_0()
                            .text_xs()
                            .text_color(colors::text_muted())
                            .text_ellipsis()
                            .overflow_hidden()
                            .child(action.command.clone()),
                    ),
                )
                .when_some(conflict_detail, |row, detail| {
                    row.child(
                        div()
                            .text_xs()
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(colors::warning())
                            .child(detail),
                    )
                }),
        )
        .child(
            div()
                .flex()
                .flex_wrap()
                .items_center()
                .gap_2()
                .when(stacked, |controls| controls.w_full().justify_between())
                .when(is_conflicting, |s| {
                    s.child(
                        div()
                            .px_2()
                            .py_1()
                            .rounded(Radii::sm())
                            .bg(colors::warning().opacity(0.16))
                            .text_xs()
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(colors::warning())
                            .child(if is_typing_key {
                                if snapshot_binding(bindings, &storage_key)
                                    .is_some_and(|binding| binding.key.eq_ignore_ascii_case("tab"))
                                {
                                    "Reserved"
                                } else {
                                    "Types text"
                                }
                            } else {
                                "Conflict"
                            }),
                    )
                })
                .child(
                    div()
                        .id(SharedString::from(capture_id))
                        .min_w(px(160.0))
                        .px_3()
                        .py_1()
                        .rounded(Radii::sm())
                        .bg(if is_capturing {
                            colors::accent()
                        } else {
                            colors::bg_hover()
                        })
                        .border_1()
                        .border_color(if is_capturing {
                            colors::accent()
                        } else if is_conflicting {
                            colors::warning()
                        } else {
                            colors::border()
                        })
                        .role(Role::Button)
                        .map(|control| {
                            crate::components::settings::widgets::settings_keyboard_control_with_focus(
                                control, cx,
                             capture_focus.clone(), )
                        })
                        .a11y_synthetic_children(
                            crate::components::ui::accessibility::apply_description,
                        )
                        .aria_label(format!("Change {} shortcut", action.name))
                        .cursor_pointer()
                        .hover(|s| {
                            if !is_capturing {
                                s.bg(colors::bg_elevated())
                            } else {
                                s
                            }
                        })
                        .on_activate(move |_, window, app| {
                            activation_focus.focus(window, app);
                            entity.update(app, |_, cx| {
                                toggle_settings_hotkey_capture(
                                    capture_storage_key.clone(),
                                    is_capturing,
                                    window,
                                    &activation_focus,
                                    cx,
                                );
                            });
                        })
                        .on_a11y_action(AccessibleAction::Click, move |_, window, app| {
                            accessible_focus.focus(window, app);
                            a11y_entity.update(app, |_, cx| {
                                toggle_settings_hotkey_capture(
                                    accessible_capture_storage_key.clone(),
                                    is_capturing,
                                    window,
                                    &accessible_focus,
                                    cx,
                                );
                            });
                        })
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(
                            div()
                                .text_sm()
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(if is_capturing {
                                    colors::text_on_accent_contrast()
                                } else {
                                    colors::text_primary()
                                })
                                .child(if is_capturing {
                                    "Press key…".to_string()
                                } else {
                                    binding_display
                                }),
                        ),
                )
                .child(
                    div()
                        .w(px(48.0))
                        .flex()
                        .justify_end()
                        .when(has_binding, |slot| {
                            slot.child(quiet_action_button(clear_id, "Clear", false, cx, {
                                let action_id = action.id.clone();
                                move |cx| {
                                    hotkeys::clear_custom_hotkey_binding(&action_id);
                                    cx.notify();
                                }
                            }))
                        }),
                )
                .child(quiet_action_button(edit_id, "Edit", false, cx, {
                    let action = action.clone();
                    move |cx| {
                        if settings_hotkey_capture_owned_by(cx.entity_id()) {
                            cancel_settings_hotkey_capture(cx.entity_id());
                        }
                        set_editor_for_custom_hotkey(action.clone());
                        cx.notify();
                    }
                }))
                .child(quiet_action_button(
                    format!("delete-hotkey-{}", action.id),
                    "Delete",
                    true,
                    cx,
                    {
                        let action_id = action.id.clone();
                        move |cx| {
                            if settings_hotkey_capture_owned_by(cx.entity_id()) {
                                cancel_settings_hotkey_capture(cx.entity_id());
                            }
                            hotkeys::delete_custom_hotkey(&action_id);
                            if custom_hotkey_editor_snapshot().draft.editing_id.as_deref()
                                == Some(action_id.as_str())
                            {
                                reset_custom_hotkey_editor();
                            }
                            cx.notify();
                        }
                    },
                )),
        )
}

fn render_editor_text_input<T: 'static>(
    field: CustomHotkeyEditorField,
    label: &'static str,
    cx: &mut Context<T>,
) -> impl IntoElement + use<T> {
    div()
        .w_full()
        .min_w_0()
        .h(px(SettingsMetrics::INPUT_MIN_HEIGHT))
        .when(field == CustomHotkeyEditorField::Command, |input| {
            input.font_family("monospace")
        })
        .when_some(hotkey_custom_input(field, cx), |input, state| {
            input.child(Input::new(&state).accessibility_label(label))
        })
}

fn render_editor_button<T: 'static, F: Fn(&mut Context<T>) + 'static>(
    label: &'static str,
    primary: bool,
    on_click: F,
    cx: &mut Context<T>,
) -> impl IntoElement + use<T, F> {
    let entity = cx.entity();
    let a11y_entity = entity.clone();
    let on_click = Rc::new(on_click);
    let accessible_on_click = on_click.clone();
    div()
        .id(SharedString::from(format!(
            "hotkey-editor-{}",
            crate::components::settings::widgets::control_slug(label)
        )))
        .px_3()
        .py_1()
        .rounded(Radii::md())
        .cursor_pointer()
        .bg(if primary {
            colors::accent()
        } else {
            colors::bg_surface()
        })
        .border_1()
        .border_color(if primary {
            colors::accent()
        } else {
            colors::border()
        })
        .text_xs()
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(if primary {
            colors::text_on_accent_contrast()
        } else {
            colors::text_secondary()
        })
        .role(Role::Button)
        .map(|control| crate::components::settings::widgets::settings_keyboard_control(control, cx))
        .a11y_synthetic_children(crate::components::ui::accessibility::apply_description)
        .aria_label(label)
        .child(label)
        .on_activate(move |_event, _window, app| {
            entity.update(app, |_, cx| on_click(cx));
        })
        .on_a11y_action(AccessibleAction::Click, move |_action, _window, app| {
            a11y_entity.update(app, |_, cx| accessible_on_click(cx));
        })
}

fn save_custom_hotkey_editor(expected_generation: u64) {
    let Some(editor) = prepare_custom_hotkey_editor(expected_generation) else {
        return;
    };
    let editing = editor.editing_id.is_some();
    let label = editor.label.trim().to_string();
    let command = editor.command.trim().to_string();

    if label.is_empty() || command.is_empty() {
        update_custom_hotkey_editor(|editor| {
            editor.status = Some("Label and command are required".to_string())
        });
        return;
    }

    let action = CustomHotkeyAction {
        id: editor
            .editing_id
            .clone()
            .or(editor.draft_id.clone())
            .unwrap_or_else(|| Uuid::new_v4().to_string()),
        name: label,
        command,
    };

    if let Some(binding) = editor.draft_binding.as_ref() {
        let error = if is_plain_typing_binding(binding) {
            Some("Typing keys, Tab and Shift+Tab are reserved for composing.".to_string())
        } else {
            hotkeys::validate_custom_hotkey_binding(&action, editor.editing_id.as_deref(), binding)
                .err()
        };
        if let Some(error) = error {
            update_custom_hotkey_editor(|editor| editor.status = Some(error));
            return;
        }
    }
    hotkeys::save_custom_hotkey(action.clone(), editor.editing_id.as_deref());
    if let Some(binding) = editor.draft_binding {
        hotkeys::set_custom_hotkey_binding(&action.id, binding);
    }
    set_custom_hotkey_editor_state(CustomHotkeyEditorState {
        status: Some(if editing {
            "Updated custom hotkey".to_string()
        } else {
            "Added custom hotkey".to_string()
        }),
        ..CustomHotkeyEditorState::default()
    });
}

fn recovered_custom_hotkey_editor_from(
    previous: &CustomHotkeyEditorState,
    actions: &[CustomHotkeyAction],
) -> CustomHotkeyEditorState {
    let mut editor = previous
        .editing_id
        .as_deref()
        .and_then(|editing_id| actions.iter().find(|action| action.id == editing_id))
        .map(|action| CustomHotkeyEditorState {
            label: action.name.clone(),
            command: action.command.clone(),
            editing_id: Some(action.id.clone()),
            ..CustomHotkeyEditorState::default()
        })
        .unwrap_or_default();
    editor.status = Some(
        "Hotkey draft was reset after an internal editor error. Review or edit it before saving."
            .to_string(),
    );
    editor
}

fn recovered_custom_hotkey_editor(previous: &CustomHotkeyEditorState) -> CustomHotkeyEditorState {
    let actions = custom_hotkeys();
    recovered_custom_hotkey_editor_from(previous, &actions)
}

fn custom_hotkey_editor_snapshot_from(
    actions: &[CustomHotkeyAction],
) -> EditorSnapshot<CustomHotkeyEditorState> {
    CUSTOM_HOTKEY_EDITOR
        .snapshot_or_reset(|previous| recovered_custom_hotkey_editor_from(previous, actions))
}

fn custom_hotkey_editor_snapshot() -> EditorSnapshot<CustomHotkeyEditorState> {
    CUSTOM_HOTKEY_EDITOR.snapshot_or_reset(recovered_custom_hotkey_editor)
}

fn prepare_custom_hotkey_editor(expected_generation: u64) -> Option<CustomHotkeyEditorState> {
    CUSTOM_HOTKEY_EDITOR.prepare_ready(expected_generation, recovered_custom_hotkey_editor)
}

fn set_custom_hotkey_editor_state(state: CustomHotkeyEditorState) {
    CUSTOM_HOTKEY_EDITOR.replace_from_authority(state);
}

fn update_custom_hotkey_editor(update: impl FnOnce(&mut CustomHotkeyEditorState)) {
    let _ = CUSTOM_HOTKEY_EDITOR.apply_user_edit(recovered_custom_hotkey_editor, update);
}

fn set_editor_for_custom_hotkey(action: CustomHotkeyAction) {
    update_custom_hotkey_editor(|editor| {
        editor.label = action.name;
        editor.command = action.command;
        editor.editing_id = Some(action.id);
        editor.draft_id = None;
        editor.draft_binding = None;
        editor.status = None;
        editor.focused = None;
    });
}

fn reset_custom_hotkey_editor() {
    set_custom_hotkey_editor_state(CustomHotkeyEditorState::default());
}

pub(super) fn reset_custom_hotkey_editor_after_settings_replace() {
    cancel_all_settings_hotkey_captures();
    set_custom_hotkey_editor_state(CustomHotkeyEditorState {
        status: Some("Hotkey draft reset after settings were replaced".to_string()),
        ..CustomHotkeyEditorState::default()
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test;
    static CAPTURE_TEST_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn conflict_retry_escape_and_close_restore_hotkey_matching() {
        let _guard = CAPTURE_TEST_LOCK.lock().unwrap();
        hotkeys::with_isolated_capture_config(hotkeys::default_hotkeys(), || {
            let owner = EntityId::from(101_u64);
            let binding = hotkeys::default_hotkeys()[&HotkeyAction::NewTab.to_string()].clone();
            hotkeys::refresh_hotkey_cache();
            start_settings_hotkey_capture(owner, HotkeyAction::ToggleSettings.to_string());
            complete_settings_hotkey_capture(binding.clone());
            assert!(hotkeys::is_capturing());
            assert!(settings_hotkey_capture_owned_by(owner));
            assert!(reject_plain_typing_capture(
                &hotkeys::HotkeyBinding::new("tab", false, false, false),
                false
            ));
            // The shell's Escape handler uses this same cancellation path.
            cancel_settings_hotkey_capture(owner);
            release_hotkey_editor_inputs_for_host(owner);
            assert!(!hotkeys::is_capturing());
            assert!(CAPTURE_REJECTION.lock().unwrap().is_none());
            let event = KeyDownEvent {
                keystroke: Keystroke {
                    key: binding.key,
                    key_char: None,
                    modifiers: Modifiers {
                        control: binding.ctrl,
                        alt: binding.alt,
                        shift: binding.shift,
                        ..Modifiers::default()
                    },
                },
                is_held: false,
                prefer_character_input: false,
            };
            assert!(
                hotkeys::match_key_event(
                    &event,
                    hotkeys::HotkeyMatchContext {
                        allow_global: true,
                        ..hotkeys::HotkeyMatchContext::default()
                    },
                    false,
                )
                .is_some()
            );
        });
    }

    #[test]
    fn host_close_cancels_conflict_even_without_active_owner_entry() {
        let _guard = CAPTURE_TEST_LOCK.lock().unwrap();
        hotkeys::with_isolated_capture_config(hotkeys::default_hotkeys(), || {
            let owner = EntityId::from(102_u64);
            let binding = hotkeys::default_hotkeys()[&HotkeyAction::NewTab.to_string()].clone();
            start_settings_hotkey_capture(owner, HotkeyAction::ToggleSettings.to_string());
            complete_settings_hotkey_capture(binding);
            HOTKEY_CAPTURE_OWNERS.with(|owners| owners.borrow_mut().clear());
            release_hotkey_editor_inputs_for_host(owner);
            assert!(!hotkeys::is_capturing());
        });
    }

    #[test]
    fn plain_tab_and_shift_tab_remain_composer_keys() {
        assert!(is_plain_typing_binding(&hotkeys::HotkeyBinding::new(
            "tab", false, false, false
        )));
        assert!(is_plain_typing_binding(&hotkeys::HotkeyBinding::new(
            "tab", false, true, false
        )));
        assert!(!is_plain_typing_binding(&hotkeys::HotkeyBinding::new(
            "tab", true, false, false
        )));
    }

    #[test]
    fn legacy_plain_tab_binding_cannot_preempt_composing() {
        let _guard = CAPTURE_TEST_LOCK.lock().unwrap();
        let mut bindings = hotkeys::default_hotkeys();
        bindings.insert(
            HotkeyAction::ToggleSettings.to_string(),
            hotkeys::HotkeyBinding::new("tab", false, false, false),
        );
        bindings.insert(
            HotkeyAction::ToggleAccountSwitcher.to_string(),
            hotkeys::HotkeyBinding::new("tab", false, true, false),
        );
        hotkeys::with_isolated_capture_config(bindings, || {
            let mut event = KeyDownEvent {
                keystroke: Keystroke {
                    key: "tab".into(),
                    ..Keystroke::default()
                },
                is_held: false,
                prefer_character_input: false,
            };
            let context = hotkeys::HotkeyMatchContext {
                allow_global: true,
                ..hotkeys::HotkeyMatchContext::default()
            };
            assert!(hotkeys::match_key_event(&event, context.clone(), true).is_none());
            assert!(hotkeys::match_key_event(&event, context.clone(), false).is_some());
            event.keystroke.modifiers.shift = true;
            assert!(hotkeys::match_key_event(&event, context.clone(), true).is_none());
            assert!(hotkeys::match_key_event(&event, context.clone(), false).is_some());
            event.keystroke.modifiers.shift = false;
            event.keystroke.modifiers.control = true;
            assert!(hotkeys::match_key_event(&event, context, true).is_some());
        });
    }

    #[test]
    fn collapsing_editor_retains_modified_only_and_cancels_capture() {
        hotkeys::with_isolated_capture_config(hotkeys::default_hotkeys(), || {
            let owner = EntityId::from(103_u64);
            HOTKEY_EDITOR_VIEWS.with(|views| {
                views
                    .borrow_mut()
                    .entry(owner)
                    .or_default()
                    .show_modified_only = true
            });
            start_settings_hotkey_capture(owner, HotkeyAction::ToggleSettings.to_string());
            release_hotkey_editor_inputs_for_host(owner);
            assert!(!hotkeys::is_capturing());
            HOTKEY_EDITOR_VIEWS.with(|views| {
                let views = views.borrow();
                let view = views.get(&owner).unwrap();
                assert!(view.show_modified_only);
                assert!(view.started_capture.is_none());
            });
            forget_hotkey_editor_host(owner);
            HOTKEY_EDITOR_VIEWS.with(|views| assert!(!views.borrow().contains_key(&owner)));
        });
    }

    #[test]
    fn programmatic_input_sync_preserves_review_and_feedback() {
        let editor = CustomHotkeyEditorState {
            label: "saved\r\nlabel".to_string(),
            status: Some("Added custom hotkey".to_string()),
            ..CustomHotkeyEditorState::default()
        };
        assert!(!custom_hotkey_text_changed(
            &editor,
            CustomHotkeyEditorField::Label,
            "saved label"
        ));
        assert!(!custom_hotkey_text_changed(
            &editor,
            CustomHotkeyEditorField::Command,
            ""
        ));
        assert!(custom_hotkey_text_changed(
            &editor,
            CustomHotkeyEditorField::Label,
            "edited label"
        ));
    }

    fn action(id: &str, name: &str, command: &str) -> CustomHotkeyAction {
        CustomHotkeyAction {
            id: id.to_string(),
            name: name.to_string(),
            command: command.to_string(),
        }
    }

    #[test]
    fn recovery_reloads_the_exact_persisted_hotkey_by_id() {
        let previous = CustomHotkeyEditorState {
            label: "partial".to_string(),
            command: "partial".to_string(),
            editing_id: Some("two".to_string()),
            focused: Some(CustomHotkeyEditorField::Command),
            status: None,
            ..CustomHotkeyEditorState::default()
        };
        let actions = [action("one", "One", "/one"), action("two", "Two", "/two")];

        let recovered = recovered_custom_hotkey_editor_from(&previous, &actions);
        assert_eq!(recovered.editing_id.as_deref(), Some("two"));
        assert_eq!(recovered.label, "Two");
        assert_eq!(recovered.command, "/two");
        assert!(recovered.focused.is_none());
        assert!(recovered.status.is_some());
    }

    #[test]
    fn recovery_uses_a_marked_new_form_when_the_hotkey_was_removed() {
        let previous = CustomHotkeyEditorState {
            editing_id: Some("removed".to_string()),
            label: "partial".to_string(),
            ..CustomHotkeyEditorState::default()
        };

        let recovered = recovered_custom_hotkey_editor_from(&previous, &[]);
        assert_eq!(recovered.editing_id, None);
        assert!(recovered.label.is_empty());
        assert!(recovered.command.is_empty());
        assert!(recovered.status.is_some());
    }

    #[test]
    fn built_in_action_descriptors_cover_every_registered_action() {
        let categories = [
            (
                HOTKEYS_NAVIGATION,
                HOTKEYS_NAVIGATION_SETTINGS,
                NAVIGATION_ACTIONS,
            ),
            (HOTKEYS_CHAT, HOTKEYS_CHAT_SETTINGS, CHAT_ACTIONS),
            (HOTKEYS_SEARCH, HOTKEYS_SEARCH_SETTINGS, SEARCH_ACTIONS),
            (
                HOTKEYS_MODERATION,
                HOTKEYS_MODERATION_SETTINGS,
                MODERATION_ACTIONS,
            ),
            (HOTKEYS_OTHER, HOTKEYS_OTHER_SETTINGS, OTHER_ACTIONS),
        ];

        for (category, descriptors, actions) in categories {
            assert_eq!(descriptors.len(), actions.len() + 1);
            for action in actions {
                let descriptor = builtin_hotkey_descriptor(*action, category);
                let expected_id = format!("hotkeys.{}", action.to_string());
                let expected_control_id = format!("hotkey-bind-{action:?}");
                assert_ne!(descriptor.id, category.id);
                assert_eq!(descriptor.id, expected_id.as_str());
                assert_eq!(descriptor.control_id, expected_control_id.as_str());
            }
        }
    }

    #[test]
    fn builtin_action_filter_matches_action_category_and_binding_text() {
        let mut bindings = HashMap::new();
        bindings.insert(
            HotkeyAction::ToggleSettings.to_string(),
            hotkeys::HotkeyBinding::new("k", true, false, false),
        );
        assert!(builtin_action_matches_query(
            HotkeyAction::ToggleSettings,
            "Navigation",
            "open settings",
            &bindings,
        ));
        assert!(builtin_action_matches_query(
            HotkeyAction::ToggleSettings,
            "Navigation",
            "navigation",
            &bindings,
        ));
        assert!(builtin_action_matches_query(
            HotkeyAction::ToggleSettings,
            "Navigation",
            "ctrl+k",
            &bindings,
        ));
        assert!(!builtin_action_matches_query(
            HotkeyAction::ToggleSettings,
            "Navigation",
            "moderation-only-term",
            &bindings,
        ));
    }

    #[test]
    fn selected_action_overrides_filter_and_modified_only() {
        assert!(builtin_action_is_visible(
            HotkeyAction::ToggleSettings,
            "Navigation",
            "does not match",
            true,
            true,
            &HashMap::new(),
            false,
        ));
    }

    #[test]
    fn new_draft_capture_target_gets_a_stable_id_without_persisting() {
        let draft = CustomHotkeyEditorState {
            label: "Reply".to_string(),
            command: "/r hello".to_string(),
            draft_id: Some("draft-id".to_string()),
            ..CustomHotkeyEditorState::default()
        };
        let action = custom_hotkey_editor_capture_target(&draft).expect("complete draft");
        assert_eq!(action.id, "draft-id");
        assert_eq!(action.name, "Reply");
        assert_eq!(action.command, "/r hello");
    }
}

/// Render the Reset All button
fn render_reset_all_button<T: 'static>(cx: &mut Context<T>) -> impl IntoElement + use<T> {
    div().w_full().flex().justify_end().pt_2().child(
        div()
            .id("hotkey-reset-all")
            .role(Role::Button)
            .map(|control| {
                crate::components::settings::widgets::settings_keyboard_control(control, cx)
            })
            .a11y_synthetic_children(crate::components::ui::accessibility::apply_description)
            .aria_label("Reset all shortcuts to defaults")
            .px_4()
            .py_2()
            .rounded(Radii::md())
            .bg(colors::bg_surface())
            .border_1()
            .border_color(colors::border())
            .cursor_pointer()
            .hover(|s| s.bg(colors::bg_hover()))
            .on_accessible_activate(|_, _, _| {
                crate::backend::push_ui_action(crate::backend::UiAction::ConfirmResetHotkeys);
            })
            .child(
                div()
                    .text_sm()
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(colors::text_secondary())
                    .child(HOTKEYS_RESET_ALL.label),
            ),
    )
}
