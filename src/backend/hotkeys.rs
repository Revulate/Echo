//! Hotkey runtime: settings-backed bindings, matching, and capture state.
//!
//! The rebindable action/scope/binding model is GPUI-free and lives in
//! `echo-core`; this module owns the settings-backed cache, conflict
//! resolution, and the GPUI keystroke adapters.

use crate::backend::lock_utils::RwLockExt;
use gpui::KeyDownEvent;
use std::collections::{HashMap, HashSet};
use std::sync::{LazyLock, RwLock};

pub use echo_core::hotkeys::*;

#[cfg(test)]
thread_local! {
    static TEST_HOTKEYS: std::cell::RefCell<Option<HashMap<String, HotkeyBinding>>> = const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
static CAPTURE_FIXTURE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn stored_hotkeys() -> HashMap<String, HotkeyBinding> {
    #[cfg(test)]
    if let Some(bindings) = TEST_HOTKEYS.with(|settings| settings.borrow().clone()) {
        return bindings;
    }
    super::settings::get_hotkeys()
}

/// In-memory capture/custom-save fixture, restoring runtime state on unwind.
/// Delete/reset collection operations and migrations are outside this contract.
#[cfg(test)]
pub(crate) fn with_isolated_capture_config(
    bindings: HashMap<String, HotkeyBinding>,
    test: impl FnOnce(),
) {
    let _guard = CAPTURE_FIXTURE_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    struct Restore {
        settings: Option<HashMap<String, HotkeyBinding>>,
        cache: Vec<CachedHotkeyEntry>,
        capture: Option<String>,
        error: Option<String>,
    }
    impl Drop for Restore {
        fn drop(&mut self) {
            TEST_HOTKEYS.with(|settings| *settings.borrow_mut() = self.settings.take());
            *HOTKEY_CACHE.write_or_recover() = std::mem::take(&mut self.cache);
            *CAPTURE_STATE.write_or_recover() = self.capture.take();
            *CAPTURE_ERROR.write_or_recover() = self.error.take();
        }
    }
    let _restore = Restore {
        settings: TEST_HOTKEYS.with(|settings| settings.replace(Some(bindings))),
        cache: HOTKEY_CACHE.read_or_recover().clone(),
        capture: CAPTURE_STATE.read_or_recover().clone(),
        error: CAPTURE_ERROR.read_or_recover().clone(),
    };
    refresh_hotkey_cache();
    test();
}

/// GPUI reports the platform (Command/Super) modifier separately; Echo has
/// always treated it as Ctrl so one binding set works on every platform.
/// Normalize a GPUI keystroke into the model's `(key, ctrl, shift, alt)` tuple.
fn keystroke_parts(event: &KeyDownEvent) -> (String, bool, bool, bool) {
    (
        event.keystroke.key.to_lowercase(),
        event.keystroke.modifiers.control || event.keystroke.modifiers.platform,
        event.keystroke.modifiers.shift,
        event.keystroke.modifiers.alt,
    )
}

/// Check if a binding matches a KeyDownEvent
pub fn binding_matches_event(binding: &HotkeyBinding, event: &KeyDownEvent) -> bool {
    let (key, ctrl, shift, alt) = keystroke_parts(event);
    binding.matches_keystroke(&key, ctrl, shift, alt)
}

#[derive(Clone, Debug)]
struct CachedHotkeyEntry {
    binding: HotkeyBinding,
    scope: HotkeyScope,
    action: MatchedHotkey,
}

/// Cached hotkey bindings for fast lookup
static HOTKEY_CACHE: LazyLock<RwLock<Vec<CachedHotkeyEntry>>> =
    LazyLock::new(|| RwLock::new(Vec::new()));

/// Hotkey capture state (which action is being rebound)
static CAPTURE_STATE: LazyLock<RwLock<Option<String>>> = LazyLock::new(|| RwLock::new(None));

/// Last capture error shown by the hotkey editor
static CAPTURE_ERROR: LazyLock<RwLock<Option<String>>> = LazyLock::new(|| RwLock::new(None));

/// Initialize hotkey cache from settings
pub fn init_hotkeys() {
    migrate_close_bindings();
    migrate_change_channel_binding();
    migrate_whispers_close_tab_conflict();
    migrate_printable_key_defaults();
    refresh_hotkey_cache();
}

/// Printable keys always type into the chat input, so Focus Input loses its
/// old `/` default and the shortcut reference moves from `?` to Ctrl+/.
///
/// Only bindings still exactly equal to those old defaults are rewritten.
fn migrate_printable_key_defaults() {
    let stored = stored_hotkeys();
    if stored.get(&HotkeyAction::FocusInput.to_string())
        == Some(&HotkeyBinding::new("/", false, false, false))
    {
        super::settings::set_hotkey(
            HotkeyAction::FocusInput.to_string(),
            HotkeyBinding::default(),
        );
    }
    if stored.get(&HotkeyAction::ToggleCheatSheet.to_string())
        == Some(&HotkeyBinding::new("?", false, true, false))
    {
        super::settings::set_hotkey(
            HotkeyAction::ToggleCheatSheet.to_string(),
            HotkeyBinding::new("/", true, false, false),
        );
    }
}

/// Unbind Toggle Whispers on profiles still holding the old default, where it
/// shared Ctrl+Shift+W with Close Tab and which one fired depended on hash order.
///
/// Only the exact pair of untouched defaults is rewritten.
fn migrate_whispers_close_tab_conflict() {
    let stored = stored_hotkeys();
    let legacy_shared = HotkeyBinding::new("w", true, true, false);
    if stored.get(&HotkeyAction::ToggleWhispers.to_string()) != Some(&legacy_shared)
        || stored.get(&HotkeyAction::CloseTab.to_string()) != Some(&legacy_shared)
    {
        return;
    }

    super::settings::set_hotkey(
        HotkeyAction::ToggleWhispers.to_string(),
        HotkeyBinding::default(),
    );
}

/// Move an existing profile onto the Ctrl+W Close Split / Ctrl+Shift+W Close
/// Tab pairing.
///
/// The legacy default bound Ctrl+W to Close Tab and had no Close Split action.
/// A profile still carrying that exact default is rewritten once; a profile that
/// customized Close Tab is left alone and simply gains the new action.
fn migrate_close_bindings() {
    let stored = stored_hotkeys();
    if stored.contains_key(&HotkeyAction::CloseSplit.to_string()) {
        return;
    }

    let legacy_close_tab = HotkeyBinding::new("w", true, false, false);
    if stored.get(&HotkeyAction::CloseTab.to_string()) != Some(&legacy_close_tab) {
        return;
    }

    super::settings::set_hotkey(
        HotkeyAction::CloseTab.to_string(),
        HotkeyBinding::new("w", true, true, false),
    );
    super::settings::set_hotkey(HotkeyAction::CloseSplit.to_string(), legacy_close_tab);
}

/// Move profiles carrying the legacy Ctrl+R Reload Emotes default onto the
/// Ctrl+R Change Channel / F5 Reload Emotes pairing.
///
/// Only the exact legacy default is rewritten. A profile that deliberately
/// rebound Reload Emotes keeps that customization and Change Channel remains
/// unbound until the user chooses a non-conflicting shortcut.
fn migrate_change_channel_binding() {
    let stored = stored_hotkeys();
    if stored.contains_key(&HotkeyAction::ChangeChannel.to_string()) {
        return;
    }

    let legacy_reload_emotes = HotkeyBinding::new("r", true, false, false);
    if stored.get(&HotkeyAction::ReloadEmotes.to_string()) != Some(&legacy_reload_emotes) {
        return;
    }

    super::settings::set_hotkey(
        HotkeyAction::ReloadEmotes.to_string(),
        HotkeyBinding::new("f5", false, false, false),
    );
    super::settings::set_hotkey(
        HotkeyAction::ChangeChannel.to_string(),
        legacy_reload_emotes,
    );
}

/// Refresh the hotkey cache from settings
pub fn refresh_hotkey_cache() {
    let mut settings_map: Vec<_> = effective_hotkey_settings().into_iter().collect();
    settings_map.sort_by(|(left, _), (right, _)| left.cmp(right));
    let mut cache = HOTKEY_CACHE.write_or_recover();
    cache.clear();

    for (action_name, binding) in settings_map {
        if let Ok(action) = action_name.parse::<HotkeyAction>() {
            cache.push(CachedHotkeyEntry {
                binding,
                scope: action.scope(),
                action: MatchedHotkey::Builtin(action),
            });
            continue;
        }

        if let Some(action) = parse_custom_hotkey_action(&action_name) {
            cache.push(CachedHotkeyEntry {
                binding,
                scope: action.scope(),
                action: MatchedHotkey::CustomCommand(action),
            });
        }
    }

    cache.sort_by_key(|right| std::cmp::Reverse(right.scope.priority()));
}

/// Match a key event to an action
pub fn match_key_event(
    event: &KeyDownEvent,
    context: HotkeyMatchContext,
    text_input_focused: bool,
) -> Option<MatchedHotkey> {
    if is_capturing() {
        return None;
    }

    let (key, ctrl, shift, alt) = keystroke_parts(event);
    // Existing plain-Tab bindings continue to work unless text input owns
    // focus, where Tab belongs to completion and focus navigation. New
    // captures still reject it in the settings editor.
    if text_input_focused && key == "tab" && !ctrl && !alt {
        return None;
    }
    let cache = HOTKEY_CACHE.read_or_recover();
    for entry in cache.iter() {
        if entry.scope.is_available(&context)
            && entry.binding.matches_keystroke(&key, ctrl, shift, alt)
        {
            return Some(entry.action.clone());
        }
    }
    None
}

/// Get binding for a specific action
pub fn get_binding(action: HotkeyAction) -> Option<HotkeyBinding> {
    binding_for_storage_key(&action.to_string())
}

/// Get binding for a custom command hotkey.
pub fn get_custom_hotkey_binding(action_id: &str) -> Option<HotkeyBinding> {
    find_custom_hotkey_entry(action_id).map(|(_, binding, _)| binding)
}

/// Apply a binding after the custom action itself has been saved.
pub fn set_custom_hotkey_binding(action_id: &str, binding: HotkeyBinding) {
    if let Some((storage_key, _, _)) = find_custom_hotkey_entry(action_id) {
        set_binding_for_storage_key(&storage_key, binding);
    }
}

/// Whether a custom command hotkey has a non-empty key, plus its display text
/// (`"Not set"` when unbound).
pub fn custom_hotkey_binding_display(action_id: &str) -> (bool, String) {
    let binding = get_custom_hotkey_binding(action_id);
    let is_set = binding
        .as_ref()
        .is_some_and(|binding| !binding.key.trim().is_empty());
    let display = binding
        .map(|binding| {
            if !binding.key.trim().is_empty() {
                binding.to_display()
            } else {
                "Not set".to_string()
            }
        })
        .unwrap_or_else(|| "Not set".to_string());
    (is_set, display)
}

/// Return all configured custom command hotkeys.
pub fn custom_hotkeys() -> Vec<CustomHotkeyAction> {
    let mut actions: Vec<_> = stored_hotkeys()
        .keys()
        .filter_map(|key| parse_custom_hotkey_action(key))
        .collect();
    actions.sort_by_cached_key(|action| action.name.to_lowercase());
    actions
}

/// Persist or update a custom hotkey action while preserving its current binding.
pub fn save_custom_hotkey(action: CustomHotkeyAction, previous_id: Option<&str>) {
    let lookup_id = previous_id.unwrap_or(&action.id);
    let old_entry = find_custom_hotkey_entry(lookup_id);
    let binding = old_entry
        .as_ref()
        .map(|(_, binding, _)| binding.clone())
        .unwrap_or_default();
    let new_key = action.storage_key();

    if let Some((old_key, _, _)) = old_entry
        && old_key != new_key
    {
        clear_binding_for_storage_key(&old_key);
    }

    set_binding_for_storage_key(&new_key, binding);
}

/// Delete a custom command hotkey definition.
pub fn delete_custom_hotkey(action_id: &str) {
    if let Some((storage_key, _, _)) = find_custom_hotkey_entry(action_id) {
        clear_binding_for_storage_key(&storage_key);
    }
}

/// Clear a custom command hotkey binding while keeping the action row.
pub fn clear_custom_hotkey_binding(action_id: &str) {
    if let Some((storage_key, _, _)) = find_custom_hotkey_entry(action_id) {
        set_binding_for_storage_key(&storage_key, HotkeyBinding::default());
    }
}

/// Lookup a stored binding by its raw action key.
pub fn binding_for_storage_key(storage_key: &str) -> Option<HotkeyBinding> {
    effective_hotkey_settings()
        .get(storage_key)
        .filter(|binding| !binding.key.trim().is_empty())
        .cloned()
}

pub struct HotkeyEditorSnapshot {
    pub bindings: HashMap<String, HotkeyBinding>,
    pub custom_actions: Vec<CustomHotkeyAction>,
    pub conflicts: HashSet<String>,
    pub conflict_labels: HashMap<String, Vec<String>>,
}

/// Parse and compare the effective configuration once for the settings view.
pub fn editor_snapshot(include_conflicts: bool) -> HotkeyEditorSnapshot {
    let bindings = effective_hotkey_settings();
    let (actions, custom_actions) = configured_action_bindings_from(&bindings, include_conflicts);
    let conflicts = if include_conflicts {
        conflict_index(&actions)
    } else {
        ConflictIndex::default()
    };
    HotkeyEditorSnapshot {
        bindings,
        custom_actions,
        conflicts: conflicts.storage_keys,
        conflict_labels: conflicts.labels,
    }
}

/// Revalidate a staged binding against the current configuration at Save.
pub fn validate_custom_hotkey_binding(
    action: &CustomHotkeyAction,
    previous_id: Option<&str>,
    binding: &HotkeyBinding,
) -> Result<(), String> {
    let key = previous_id
        .and_then(find_custom_hotkey_entry)
        .map(|(key, _, _)| key)
        .unwrap_or_else(|| action.storage_key());
    let conflicts = conflicting_labels_for_new_binding(&key, binding);
    if conflicts.is_empty() {
        return Ok(());
    }
    Err(format!(
        "{} is already used by {}",
        binding.to_display(),
        conflicts.join(", ")
    ))
}

fn parse_custom_hotkey_action(storage_key: &str) -> Option<CustomHotkeyAction> {
    storage_key
        .strip_prefix(CUSTOM_HOTKEY_PREFIX)
        .and_then(|json| serde_json::from_str(json).ok())
}

#[derive(Clone, Debug)]
struct ConfiguredActionBinding {
    storage_key: String,
    scope: HotkeyScope,
    label: String,
    conflict_key: Option<String>,
}

#[derive(Default)]
struct ConflictIndex {
    storage_keys: HashSet<String>,
    labels: HashMap<String, Vec<String>>,
}

/// Compute each binding's normalized conflict key once, then compare only
/// actions in the same conflict-key group. All display and mutation checks
/// use this index so their reachable-scope semantics stay identical.
fn conflict_index(actions: &[ConfiguredActionBinding]) -> ConflictIndex {
    let mut by_conflict_key: HashMap<&str, Vec<&ConfiguredActionBinding>> = HashMap::new();
    for action in actions {
        if let Some(conflict_key) = action.conflict_key.as_deref() {
            by_conflict_key
                .entry(conflict_key)
                .or_default()
                .push(action);
        }
    }

    let mut index = ConflictIndex::default();
    for group in by_conflict_key.into_values() {
        for (left_index, left) in group.iter().enumerate() {
            for right in group.iter().skip(left_index + 1) {
                if !left.scope.conflicts_with(right.scope) {
                    continue;
                }
                index.storage_keys.insert(left.storage_key.clone());
                index.storage_keys.insert(right.storage_key.clone());
                index
                    .labels
                    .entry(left.storage_key.clone())
                    .or_default()
                    .push(right.label.clone());
                index
                    .labels
                    .entry(right.storage_key.clone())
                    .or_default()
                    .push(left.label.clone());
            }
        }
    }
    for labels in index.labels.values_mut() {
        labels.sort();
        labels.dedup();
    }
    index
}

fn configured_action_bindings() -> Vec<ConfiguredActionBinding> {
    configured_action_bindings_from(&effective_hotkey_settings(), true).0
}

fn configured_action_bindings_from(
    bindings: &HashMap<String, HotkeyBinding>,
    include_conflict_keys: bool,
) -> (Vec<ConfiguredActionBinding>, Vec<CustomHotkeyAction>) {
    let mut actions = Vec::new();
    let mut custom_actions = Vec::new();

    for (storage_key, binding) in bindings {
        if let Ok(action) = storage_key.parse::<HotkeyAction>() {
            actions.push(ConfiguredActionBinding {
                storage_key: storage_key.clone(),
                scope: action.scope(),
                label: action.display_name().to_string(),
                conflict_key: include_conflict_keys
                    .then(|| binding.conflict_key())
                    .flatten(),
            });
        } else if let Some(action) = parse_custom_hotkey_action(storage_key) {
            actions.push(ConfiguredActionBinding {
                storage_key: storage_key.clone(),
                scope: action.scope(),
                label: action.name.clone(),
                conflict_key: include_conflict_keys
                    .then(|| binding.conflict_key())
                    .flatten(),
            });
            custom_actions.push(action);
        }
    }

    custom_actions.sort_by_cached_key(|action| action.name.to_lowercase());
    (actions, custom_actions)
}

fn find_custom_hotkey_entry(
    action_id: &str,
) -> Option<(String, HotkeyBinding, CustomHotkeyAction)> {
    stored_hotkeys()
        .into_iter()
        .find_map(|(storage_key, binding)| {
            let action = parse_custom_hotkey_action(&storage_key)?;
            (action.id == action_id).then_some((storage_key, binding, action))
        })
}

fn conflicting_labels_for_new_binding(storage_key: &str, binding: &HotkeyBinding) -> Vec<String> {
    let Some(target_scope) = storage_key_scope(storage_key) else {
        return Vec::new();
    };
    let mut actions = configured_action_bindings();
    actions.retain(|action| action.storage_key != storage_key);
    actions.push(ConfiguredActionBinding {
        storage_key: storage_key.to_string(),
        scope: target_scope,
        label: String::new(),
        conflict_key: binding.conflict_key(),
    });
    let mut conflicts = conflict_index(&actions);
    conflicts.labels.remove(storage_key).unwrap_or_default()
}

fn storage_key_scope(storage_key: &str) -> Option<HotkeyScope> {
    storage_key
        .parse::<HotkeyAction>()
        .ok()
        .map(|action| action.scope())
        .or_else(|| parse_custom_hotkey_action(storage_key).map(|action| action.scope()))
}

fn effective_hotkey_settings() -> HashMap<String, HotkeyBinding> {
    let mut settings = stored_hotkeys();
    let defaults = default_hotkeys();

    for action in backfilled_default_actions() {
        let storage_key = action.to_string();
        if !settings.contains_key(&storage_key)
            && let Some(binding) = defaults.get(&storage_key)
        {
            let has_conflict = binding.conflict_key().is_some_and(|candidate_key| {
                settings.iter().any(|(existing_key, existing_binding)| {
                    let Some(existing_conflict_key) = existing_binding.conflict_key() else {
                        return false;
                    };
                    if existing_conflict_key != candidate_key {
                        return false;
                    }

                    let Some(existing_scope) = storage_key_scope(existing_key) else {
                        return false;
                    };
                    action.scope().conflicts_with(existing_scope)
                })
            });
            if !has_conflict {
                settings.insert(storage_key, binding.clone());
            }
        }
    }

    settings
}

fn backfilled_default_actions() -> &'static [HotkeyAction] {
    &[
        HotkeyAction::SearchPreviousResult,
        HotkeyAction::SearchNextResult,
        HotkeyAction::SearchOpenSelectedResult,
        HotkeyAction::SearchPreviousPage,
        HotkeyAction::SearchNextPage,
        HotkeyAction::SearchFirstResult,
        HotkeyAction::SearchLastResult,
        HotkeyAction::SearchClearQuery,
        HotkeyAction::ToggleGlobalSearch,
        HotkeyAction::NewTab,
        HotkeyAction::NewSplit,
        HotkeyAction::CloseSplit,
        HotkeyAction::MoveTabLeft,
        HotkeyAction::MoveTabRight,
        HotkeyAction::OpenTabMenu,
        HotkeyAction::RenameTab,
        HotkeyAction::SelectTab1,
        HotkeyAction::SelectTab2,
        HotkeyAction::SelectTab3,
        HotkeyAction::SelectTab4,
        HotkeyAction::SelectTab5,
        HotkeyAction::SelectTab6,
        HotkeyAction::SelectTab7,
        HotkeyAction::SelectTab8,
        HotkeyAction::SelectTab9,
        HotkeyAction::PopupActiveSplit,
        HotkeyAction::PopupActiveTab,
        HotkeyAction::ZoomIn,
        HotkeyAction::ZoomOut,
        HotkeyAction::ResetZoom,
        HotkeyAction::ToggleTabBarVisibility,
        HotkeyAction::ToggleLiveTabsOnly,
        HotkeyAction::SendMessageKeepInput,
        HotkeyAction::ChangeChannel,
        HotkeyAction::CreateClip,
        HotkeyAction::OpenChannelInStreamlink,
        HotkeyAction::OpenChannelInCustomPlayer,
        HotkeyAction::OpenModViewInBrowser,
        HotkeyAction::ReconnectActiveChannel,
        HotkeyAction::ScrollToTop,
        HotkeyAction::ScrollToBottom,
        HotkeyAction::ReloadEmotes,
        HotkeyAction::OpenDebugPopup,
        HotkeyAction::ToggleStreamerOverlay,
        HotkeyAction::ReopenClosedSplit,
        HotkeyAction::ReopenClosedTab,
        HotkeyAction::ToggleLocalR9K,
        HotkeyAction::FocusSplitLeft,
        HotkeyAction::FocusSplitRight,
        HotkeyAction::FocusSplitUp,
        HotkeyAction::FocusSplitDown,
    ]
}

fn uses_backfilled_default(action: HotkeyAction) -> bool {
    backfilled_default_actions().contains(&action)
}

fn set_binding_for_storage_key(storage_key: &str, binding: HotkeyBinding) {
    #[cfg(test)]
    if TEST_HOTKEYS.with(|settings| {
        let mut settings = settings.borrow_mut();
        if let Some(settings) = settings.as_mut() {
            settings.insert(storage_key.to_string(), binding.clone());
            true
        } else {
            false
        }
    }) {
        refresh_hotkey_cache();
        return;
    }

    super::settings::set_hotkey(storage_key.to_string(), binding);
    refresh_hotkey_cache();
}

fn clear_binding_for_storage_key(storage_key: &str) {
    #[cfg(test)]
    if TEST_HOTKEYS.with(|settings| {
        let mut settings = settings.borrow_mut();
        if let Some(settings) = settings.as_mut() {
            settings.remove(storage_key);
            true
        } else {
            false
        }
    }) {
        refresh_hotkey_cache();
        return;
    }

    super::settings::clear_hotkey(storage_key);
    refresh_hotkey_cache();
}

/// Set binding for a specific action
pub fn set_binding(action: HotkeyAction, binding: HotkeyBinding) {
    set_binding_for_storage_key(&action.to_string(), binding);
}

/// Clear binding for a specific action
pub fn clear_binding(action: HotkeyAction) {
    if uses_backfilled_default(action) {
        set_binding(action, HotkeyBinding::default());
    } else {
        super::settings::clear_hotkey(&action.to_string());
        refresh_hotkey_cache();
    }
}

/// Reset a single action to default
pub fn reset_binding(action: HotkeyAction) {
    let defaults = default_hotkeys();
    if let Some(default_binding) = defaults.get(&action.to_string()) {
        set_binding(action, default_binding.clone());
    }
}

/// Reset all bindings to defaults
pub fn reset_all_bindings() {
    let custom_entries: Vec<_> = stored_hotkeys()
        .into_iter()
        .filter(|(storage_key, _)| parse_custom_hotkey_action(storage_key).is_some())
        .collect();
    super::settings::reset_hotkeys_to_default();
    for (storage_key, binding) in custom_entries {
        super::settings::set_hotkey(storage_key, binding);
    }
    refresh_hotkey_cache();
}

/// Start capturing a new binding for any storage key.
pub fn start_hotkey_capture_for_key(storage_key: String) {
    *CAPTURE_ERROR.write_or_recover() = None;
    *CAPTURE_STATE.write_or_recover() = Some(storage_key);
}

/// Get the most recent capture error
pub fn get_capture_error() -> Option<String> {
    CAPTURE_ERROR.read_or_recover().clone()
}

/// Get the raw storage key currently being captured.
pub fn get_capturing_storage_key() -> Option<String> {
    CAPTURE_STATE.read_or_recover().clone()
}

/// Check if we're in capture mode
pub fn is_capturing() -> bool {
    CAPTURE_STATE.read_or_recover().is_some()
}

/// Cancel capture mode
pub fn cancel_capture() {
    *CAPTURE_ERROR.write_or_recover() = None;
    *CAPTURE_STATE.write_or_recover() = None;
}

/// Complete capture with the new binding
pub fn complete_capture(binding: HotkeyBinding) {
    if !validate_captured_binding(&binding) {
        return;
    }
    if let Some(storage_key) = get_capturing_storage_key() {
        set_binding_for_storage_key(&storage_key, binding);
        *CAPTURE_ERROR.write_or_recover() = None;
    }
    cancel_capture();
}

/// Run the existing capture conflict check without persisting a draft binding.
pub fn validate_captured_binding(binding: &HotkeyBinding) -> bool {
    let Some(storage_key) = get_capturing_storage_key() else {
        return true;
    };
    let conflicts = conflicting_labels_for_new_binding(&storage_key, binding);
    if conflicts.is_empty() {
        return true;
    }
    *CAPTURE_ERROR.write_or_recover() = Some(format!(
        "{} is already used by {}",
        binding.to_display(),
        conflicts.join(", ")
    ));
    false
}

/// Create binding from a key event (for capture mode)
pub fn binding_from_event(event: &KeyDownEvent) -> Option<HotkeyBinding> {
    let (key, ctrl, shift, alt) = keystroke_parts(event);

    if matches!(key.as_str(), "control" | "shift" | "alt" | "meta") {
        return None;
    }

    Some(HotkeyBinding {
        key,
        ctrl,
        shift,
        alt,
    })
}

/// Resolve hotkey variables in a command string
///
/// Supported variables:
/// - `{input.text}` - Current input text
/// - `{channel}` - Current channel name
/// - `{user}` - Current username
/// - `{time}` - Current time in HH:MM:SS format
///
/// # Example
/// ```ignore
/// let cmd = "/ban {user}";
/// let resolved = resolve_hotkey_variables(cmd, "", "twitch_channel", "streamer");
/// assert_eq!(resolved, "/ban streamer");
/// ```
pub fn resolve_hotkey_variables(
    command: &str,
    input_text: &str,
    channel: &str,
    username: &str,
) -> String {
    let now = chrono::Local::now();
    let time_str = now.format("%H:%M:%S").to_string();

    command
        .replace("{input.text}", input_text)
        .replace("{channel}", channel)
        .replace("{user}", username)
        .replace("{time}", &time_str)
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test;

    fn action(
        storage_key: &str,
        scope: HotkeyScope,
        label: &str,
        conflict_key: Option<&str>,
    ) -> ConfiguredActionBinding {
        ConfiguredActionBinding {
            storage_key: storage_key.to_string(),
            scope,
            label: label.to_string(),
            conflict_key: conflict_key.map(str::to_string),
        }
    }

    #[test]
    fn conflict_index_groups_precomputed_keys_with_reachable_scopes() {
        let actions = vec![
            action("global", HotkeyScope::Global, "Global", Some("ctrl+k")),
            action(
                "active",
                HotkeyScope::ActiveSplit,
                "Active split",
                Some("ctrl+k"),
            ),
            action("search", HotkeyScope::SearchModal, "Search", Some("ctrl+k")),
            action("other", HotkeyScope::Global, "Other", Some("ctrl+l")),
        ];

        let index = conflict_index(&actions);

        assert_eq!(
            index.storage_keys,
            HashSet::from([
                "global".to_string(),
                "active".to_string(),
                "search".to_string(),
            ])
        );
        assert_eq!(index.labels["active"], vec!["Global".to_string()]);
    }

    #[test]
    fn isolated_capture_config_saves_custom_hotkeys_in_memory() {
        with_isolated_capture_config(HashMap::new(), || {
            let action = CustomHotkeyAction {
                id: "fixture".to_string(),
                name: "Fixture action".to_string(),
                command: "/fixture".to_string(),
            };

            save_custom_hotkey(action.clone(), None);

            assert_eq!(custom_hotkeys(), vec![action]);
        });
    }
}
