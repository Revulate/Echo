//! Import from Chatterino: discovery, the settings panel's preview state,
//! staging a plan against Echo's settings and tabs, and the disk writes.
//!
//! Parsing lives in `echo_core::chatterino_import`. Applying a staged import
//! needs the live tab list, so `EchoApp` drains [`take_pending_apply`], stages
//! the request, runs [`write_import`] off the UI thread and only then changes
//! the live settings and tabs. This module queues requests and records results.

use crate::backend::lock_utils::MutexExt;
use crate::backend::mod_buttons::ModButtonConfig;
use crate::backend::settings::{
    self, AppSettings, CustomCommand, FilterAction, FilterRule, HighlightBlacklistUser,
    HighlightRule, HighlightRuleTarget, ImportedCollections, NicknameRule,
};
use crate::state::TabState;
pub use echo_core::chatterino_import::{
    HighlightKind, IgnoreAction, ImportGroup, ImportNote, ImportOutcome, ImportPlan,
    ImportSelection, SkippedItem, TabImportMode,
};
use echo_core::chatterino_import::{
    Platform, default_roots, discover, layout_channel_key, load_plan, mod_button_key,
    settings_dir_for,
};
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;

/// Explicit Chatterino folder to import from. It names a path on purpose, so
/// it is honoured even while a benchmark profile disables auto-detection.
pub const IMPORT_DIR_ENV: &str = "ECHO_CHATTERINO_IMPORT_DIR";

/// Settings row the first-run card opens. It sits inside the import
/// disclosure, so an explicit request for it expands that disclosure too.
pub const SETTINGS_TARGET_ID: &str = "developer-tools.chatterino-import-folder";

/// Quiet scope line shown in the panel and on the first-run card.
pub const NOT_IMPORTED_NOTE: &str = "Sign-in and Chatterino's other preferences aren't imported.";

const WRITE_TIMEOUT: Duration = Duration::from_secs(5);

/// How the panel found its folder.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceOrigin {
    Override,
    Detected,
    Chosen,
}

fn override_dir() -> Option<PathBuf> {
    echo_core::env::os_value(IMPORT_DIR_ENV)
        .filter(|dir| !dir.is_empty())
        .map(PathBuf::from)
}

/// The Chatterino settings folder to offer, from `ECHO_CHATTERINO_IMPORT_DIR`
/// or the OS install locations (skipped under a benchmark profile).
pub fn discover_settings_dir() -> Option<(PathBuf, SourceOrigin)> {
    if let Some(dir) = override_dir() {
        let settings_dir = settings_dir_for(&dir).unwrap_or(dir);
        return Some((settings_dir, SourceOrigin::Override));
    }
    let data_dir = crate::storage_roots::external_data_dir()?;
    let home_dir = dirs::home_dir();
    let roots = default_roots(Platform::current(), Some(&data_dir), home_dir.as_deref());
    discover(&roots).map(|dir| (dir, SourceOrigin::Detected))
}

/// The folder for the first-run card: only on a run that started without an
/// Echo settings file, and never after the card was dismissed or an import
/// ran. A benchmark profile or fixture hides it unless
/// `ECHO_CHATTERINO_IMPORT_DIR` names a folder, which keeps it testable.
pub fn first_run_prompt_dir() -> Option<PathBuf> {
    let isolated = crate::storage_roots::is_isolated() || crate::bench::ui_fixture::is_enabled();
    if (isolated && override_dir().is_none())
        || !settings::settings_file_was_missing_at_startup()
        || settings::get_chatterino_import_prompt_dismissed()
    {
        return None;
    }
    discover_settings_dir()
        .map(|(dir, _)| dir)
        .filter(|dir| settings_dir_for(dir).is_some())
}

/// Result line under the Import button.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImportStatus {
    pub message: String,
    pub is_error: bool,
}

/// What the settings panel shows. Shared by the modal and the popout.
#[derive(Clone, Debug, Default)]
pub struct ImportPanelState {
    pub scanned: bool,
    pub scanning: bool,
    scan_generation: u64,
    pub source: Option<(PathBuf, SourceOrigin)>,
    pub plan: Option<Arc<ImportPlan>>,
    pub selection: ImportSelection,
    /// An import is waiting for confirmation or running.
    pub busy: bool,
    pub status: Option<ImportStatus>,
}

impl ImportPanelState {
    /// Groups that would import at least one entry.
    pub fn has_selected_items(&self) -> bool {
        self.plan.as_ref().is_some_and(|plan| {
            ImportGroup::ALL
                .iter()
                .any(|group| self.selection.includes(*group) && plan.count(*group) > 0)
        })
    }

    /// Import can start now.
    pub fn can_import(&self) -> bool {
        !self.busy && !self.scanning && self.has_selected_items()
    }

    /// The folder a rescan should read: one picked by hand stays picked.
    pub fn rescan_dir(&self) -> Option<PathBuf> {
        self.source
            .as_ref()
            .filter(|(_, origin)| *origin == SourceOrigin::Chosen)
            .map(|(dir, _)| dir.clone())
    }
}

static PANEL: LazyLock<Mutex<ImportPanelState>> =
    LazyLock::new(|| Mutex::new(ImportPanelState::default()));

pub fn panel_snapshot() -> ImportPanelState {
    PANEL.lock_or_recover().clone()
}

pub fn set_group_selected(group: ImportGroup, selected: bool) {
    PANEL.lock_or_recover().selection.set(group, selected);
}

pub fn set_tab_mode(mode: TabImportMode) {
    PANEL.lock_or_recover().selection.tab_mode = mode;
}

pub fn set_status(status: Option<ImportStatus>) {
    PANEL.lock_or_recover().status = status;
}

/// Mark a scan as started and return its generation. A newer scan
/// supersedes a running one, whose result is then dropped. A scan the user
/// asked for clears the last result line; an automatic one keeps it.
pub fn begin_scan(clear_status: bool) -> u64 {
    let mut panel = PANEL.lock_or_recover();
    panel.scanning = true;
    panel.scan_generation = panel.scan_generation.wrapping_add(1);
    if clear_status {
        panel.status = None;
    }
    panel.scan_generation
}

/// Outcome of a background scan.
pub struct ScanResult {
    source: Option<(PathBuf, SourceOrigin)>,
    plan: Option<ImportPlan>,
}

/// Find (or use `chosen`) and parse a Chatterino folder. Blocking file I/O:
/// run it off the UI thread.
pub fn run_scan(chosen: Option<PathBuf>) -> ScanResult {
    let source = match chosen {
        Some(dir) => Some((settings_dir_for(&dir).unwrap_or(dir), SourceOrigin::Chosen)),
        None => discover_settings_dir(),
    };
    let plan = source.as_ref().map(|(dir, _)| load_plan(dir));
    ScanResult { source, plan }
}

pub fn finish_scan(generation: u64, result: ScanResult) {
    let mut panel = PANEL.lock_or_recover();
    if panel.scan_generation != generation {
        return;
    }
    panel.scanning = false;
    panel.scanned = true;
    panel.source = result.source;
    panel.plan = result.plan.map(Arc::new);
}

/// A plan and the user's choices, queued for `EchoApp`.
#[derive(Clone)]
pub struct ImportRequest {
    pub plan: Arc<ImportPlan>,
    pub selection: ImportSelection,
    /// The user already agreed to replace their tabs.
    pub replace_confirmed: bool,
}

static PENDING_CONFIRM: Mutex<Option<ImportRequest>> = Mutex::new(None);
static PENDING_APPLY: Mutex<Option<ImportRequest>> = Mutex::new(None);

/// Start importing the panel's preview.
pub fn request_import() -> Result<(), String> {
    let request = {
        let mut panel = PANEL.lock_or_recover();
        if panel.busy {
            return Err("An import is already running.".to_string());
        }
        if panel.scanning {
            return Err("Wait for the Chatterino folder to finish reading.".to_string());
        }
        let Some(plan) = panel.plan.clone() else {
            return Err("No Chatterino setup has been read yet.".to_string());
        };
        if !panel.has_selected_items() {
            return Err("Choose at least one group that has something to import.".to_string());
        }
        panel.busy = true;
        panel.status = None;
        ImportRequest {
            plan,
            selection: panel.selection,
            replace_confirmed: false,
        }
    };
    queue_apply(request);
    Ok(())
}

/// Hold a staged replace until the user answers; `tab_count` is what will
/// actually be opened.
pub fn ask_to_replace(request: ImportRequest, tab_count: usize) {
    *PENDING_CONFIRM.lock_or_recover() = Some(request);
    crate::backend::push_ui_action(crate::backend::UiAction::ConfirmChatterinoTabReplace {
        tab_count,
    });
}

/// The user confirmed "Replace my tabs".
pub fn confirm_pending_replace() {
    if let Some(mut request) = PENDING_CONFIRM.lock_or_recover().take() {
        request.replace_confirmed = true;
        queue_apply(request);
    }
}

/// The user backed out of "Replace my tabs".
pub fn cancel_pending_replace() {
    if PENDING_CONFIRM.lock_or_recover().take().is_some() {
        let mut panel = PANEL.lock_or_recover();
        panel.busy = false;
        panel.status = Some(ImportStatus {
            message: "Import cancelled; nothing was changed.".to_string(),
            is_error: false,
        });
    }
}

fn queue_apply(request: ImportRequest) {
    *PENDING_APPLY.lock_or_recover() = Some(request);
    crate::managers::frame_rate_controller::record_activity();
}

pub fn take_pending_apply() -> Option<ImportRequest> {
    PENDING_APPLY.lock_or_recover().take()
}

/// Why an import did not happen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ImportError {
    /// Chatterino's files changed after the preview was read.
    SourcesChanged,
    Failed(String),
}

impl ImportError {
    pub fn message(&self) -> String {
        match self {
            ImportError::SourcesChanged => {
                "Chatterino's files changed since the preview; review again.".to_string()
            }
            ImportError::Failed(message) => message.clone(),
        }
    }
}

/// Record how an import ended. Success retires the first-run card; changed
/// source files send the panel back to scanning.
pub fn finish_import(result: &Result<ImportOutcome, ImportError>) {
    let status = match result {
        Ok(outcome) => {
            settings::set_chatterino_import_prompt_dismissed(true);
            ImportStatus {
                message: outcome.summary(),
                is_error: false,
            }
        }
        Err(error) => ImportStatus {
            message: error.message(),
            is_error: true,
        },
    };
    let mut panel = PANEL.lock_or_recover();
    panel.busy = false;
    panel.status = Some(status);
    if matches!(result, Err(ImportError::SourcesChanged)) {
        // The open panel rescans on its next render; a closed one on expand.
        panel.scanned = false;
    }
}

/// An import converted to Echo's types and checked against what Echo has.
pub struct StagedImport {
    pub collections: ImportedCollections,
    pub tabs: Vec<TabState>,
    /// Index into `tabs` of the tab Chatterino had selected.
    pub selected_tab: Option<usize>,
    pub tab_mode: TabImportMode,
    pub outcome: ImportOutcome,
}

impl StagedImport {
    pub fn is_empty(&self) -> bool {
        self.tabs.is_empty() && self.collections.is_empty()
    }
}

fn new_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

fn same_pattern(a: &str, a_case: bool, b: &str, b_case: bool) -> bool {
    let (a, b) = (a.trim(), b.trim());
    if a_case && b_case {
        a == b
    } else {
        a.eq_ignore_ascii_case(b)
    }
}

/// Count `candidate` against what Echo has and what this import already
/// added: `true` means import it.
fn is_new<T>(
    outcome: &mut ImportOutcome,
    group: ImportGroup,
    existing: &[T],
    staged: &[T],
    same: impl Fn(&T) -> bool,
) -> bool {
    if existing.iter().any(&same) {
        outcome.already_present[group.index()] += 1;
        false
    } else if staged.iter().any(&same) {
        outcome.repeated[group.index()] += 1;
        false
    } else {
        true
    }
}

/// Convert the selected groups to Echo entries, leaving out what Echo already
/// has and what Chatterino lists twice. Reads the current settings; changes
/// nothing.
pub fn stage(request: &ImportRequest, current_tabs: &[TabState]) -> StagedImport {
    let plan = &request.plan;
    let selection = request.selection;
    let mut collections = ImportedCollections::default();
    let mut outcome = ImportOutcome::default();
    let mut tabs = Vec::new();
    let mut selected_tab = None;

    outcome.skipped.extend(plan.file_problems().cloned());
    for group in ImportGroup::ALL {
        if selection.includes(group) {
            outcome.skipped.extend(plan.skipped_in(group).cloned());
        }
    }

    if selection.includes(ImportGroup::Tabs) {
        let existing: HashSet<Vec<String>> = match selection.tab_mode {
            TabImportMode::Append => current_tabs
                .iter()
                .map(|tab| layout_channel_key(&tab.layout))
                .collect(),
            TabImportMode::Replace => HashSet::new(),
        };
        let mut imported_keys = HashSet::new();
        for imported in &plan.tabs {
            let key = layout_channel_key(&imported.layout);
            if existing.contains(&key) {
                outcome.already_present[ImportGroup::Tabs.index()] += 1;
                continue;
            }
            if !imported_keys.insert(key) {
                outcome.repeated[ImportGroup::Tabs.index()] += 1;
                continue;
            }
            let mut tab = TabState::with_layout(imported.layout.with_fresh_split_ids());
            tab.custom_title = imported.title.clone();
            tab.highlights_enabled = imported.highlights_enabled;
            if imported.selected {
                selected_tab = Some(tabs.len());
            }
            tabs.push(tab);
        }
    }

    if selection.includes(ImportGroup::Highlights) {
        let existing = settings::get_highlight_rules();
        for imported in &plan.highlights {
            let target = match imported.kind {
                HighlightKind::Phrase => HighlightRuleTarget::Phrase,
                HighlightKind::User => HighlightRuleTarget::User,
                HighlightKind::Badge => HighlightRuleTarget::Badge,
            };
            let same = |rule: &HighlightRule| {
                rule.target == target
                    && rule.regex == imported.regex
                    && same_pattern(
                        &rule.pattern,
                        rule.case_sensitive,
                        &imported.pattern,
                        imported.case_sensitive,
                    )
            };
            if !is_new(
                &mut outcome,
                ImportGroup::Highlights,
                &existing,
                &collections.highlight_rules,
                same,
            ) {
                continue;
            }
            collections.highlight_rules.push(HighlightRule {
                id: new_id(),
                target,
                pattern: imported.pattern.clone(),
                regex: imported.regex,
                case_sensitive: imported.case_sensitive,
                show_in_mentions: imported.show_in_mentions,
                play_sound: imported.play_sound,
                alert: imported.alert,
                // Network and device paths are refused like a newly picked sound.
                sound_path: imported
                    .sound_path
                    .clone()
                    .filter(|path| !settings::is_remote_or_device_path(path)),
                enabled: true,
                color: imported.color.clone(),
            });
        }
    }

    if selection.includes(ImportGroup::HighlightIgnoredUsers) {
        let existing = settings::get_highlight_blacklist_users();
        for imported in &plan.highlight_ignored_users {
            let same = |user: &HighlightBlacklistUser| {
                user.regex == imported.regex
                    && user.pattern.trim().eq_ignore_ascii_case(&imported.pattern)
            };
            if !is_new(
                &mut outcome,
                ImportGroup::HighlightIgnoredUsers,
                &existing,
                &collections.highlight_blacklist_users,
                same,
            ) {
                continue;
            }
            collections
                .highlight_blacklist_users
                .push(HighlightBlacklistUser {
                    id: new_id(),
                    pattern: imported.pattern.clone(),
                    regex: imported.regex,
                    enabled: true,
                });
        }
    }

    let existing_filters = settings::get_filters();
    if selection.includes(ImportGroup::IgnoredPhrases) {
        for imported in &plan.ignored_phrases {
            let rule = match &imported.action {
                IgnoreAction::Block => FilterRule {
                    id: new_id(),
                    expression: imported.hide_filter_expression(),
                    action: FilterAction::Hide,
                    enabled: true,
                    replacement: None,
                    pattern_is_regex: false,
                },
                IgnoreAction::Replace(replacement) => {
                    let (expression, pattern_is_regex) = imported.replace_pattern();
                    FilterRule {
                        id: new_id(),
                        expression,
                        action: FilterAction::Replace,
                        enabled: true,
                        replacement: (!replacement.is_empty()).then(|| replacement.clone()),
                        pattern_is_regex,
                    }
                }
            };
            let same = |filter: &FilterRule| {
                filter.action == rule.action
                    && filter.expression.trim() == rule.expression.trim()
                    && filter.pattern_is_regex == rule.pattern_is_regex
                    && filter.replacement.as_deref().unwrap_or("")
                        == rule.replacement.as_deref().unwrap_or("")
            };
            if !is_new(
                &mut outcome,
                ImportGroup::IgnoredPhrases,
                &existing_filters,
                &collections.filters,
                same,
            ) {
                continue;
            }
            collections.filters.push(rule);
            outcome.imported[ImportGroup::IgnoredPhrases.index()] += 1;
        }
    }

    if selection.includes(ImportGroup::Filters) {
        for imported in &plan.filters {
            let same = |filter: &FilterRule| {
                filter.action != FilterAction::Replace
                    && filter.expression.trim() == imported.expression
            };
            if !is_new(
                &mut outcome,
                ImportGroup::Filters,
                &existing_filters,
                &collections.filters,
                same,
            ) {
                continue;
            }
            // Off by default: a Chatterino filter scoped one split, and Echo's
            // apply to every channel.
            collections.filters.push(FilterRule {
                id: new_id(),
                expression: imported.expression.clone(),
                action: FilterAction::Allow,
                enabled: false,
                replacement: None,
                pattern_is_regex: false,
            });
            outcome.imported[ImportGroup::Filters.index()] += 1;
        }
    }

    if selection.includes(ImportGroup::Nicknames) {
        let existing = settings::get_nickname_rules();
        for imported in &plan.nicknames {
            let same = |rule: &NicknameRule| {
                rule.user_id.is_none()
                    && rule.regex == imported.regex
                    && same_pattern(
                        &rule.pattern,
                        rule.case_sensitive,
                        &imported.name,
                        imported.case_sensitive,
                    )
            };
            if !is_new(
                &mut outcome,
                ImportGroup::Nicknames,
                &existing,
                &collections.nicknames,
                same,
            ) {
                continue;
            }
            collections.nicknames.push(NicknameRule {
                id: new_id(),
                pattern: imported.name.clone(),
                replacement: imported.replacement.clone(),
                user_id: None,
                regex: imported.regex,
                case_sensitive: imported.case_sensitive,
                enabled: true,
            });
        }
    }

    if selection.includes(ImportGroup::Commands) {
        let existing = settings::get_custom_commands();
        for imported in &plan.commands {
            let builtin = imported
                .name
                .strip_prefix('/')
                .and_then(|name| name.split_whitespace().next())
                .filter(|name| crate::backend::commands::is_builtin_command(name));
            if let Some(builtin) = builtin {
                outcome.skipped.push(SkippedItem {
                    group: Some(ImportGroup::Commands),
                    item: imported.name.clone(),
                    reason: format!("Echo's built-in /{builtin} command runs instead"),
                });
                continue;
            }
            let same = |command: &CustomCommand| command.name.trim() == imported.name;
            if !is_new(
                &mut outcome,
                ImportGroup::Commands,
                &existing,
                &collections.custom_commands,
                same,
            ) {
                continue;
            }
            collections.custom_commands.push(CustomCommand {
                id: new_id(),
                name: imported.name.clone(),
                response: imported.text.clone(),
                enabled: true,
            });
        }
    }

    if selection.includes(ImportGroup::ModButtons) {
        let existing = settings::get_mod_buttons();
        for imported in &plan.mod_buttons {
            let key = mod_button_key(&imported.command);
            let same = |button: &ModButtonConfig| mod_button_key(&button.command) == key;
            if !is_new(
                &mut outcome,
                ImportGroup::ModButtons,
                &existing,
                &collections.mod_buttons,
                same,
            ) {
                continue;
            }
            collections.mod_buttons.push(ModButtonConfig {
                label: imported.label.clone(),
                command: imported.command.clone(),
                icon: None,
                is_destructive: imported.destructive,
            });
        }
    }

    for (group, count) in [
        (ImportGroup::Tabs, tabs.len()),
        (ImportGroup::Highlights, collections.highlight_rules.len()),
        (
            ImportGroup::HighlightIgnoredUsers,
            collections.highlight_blacklist_users.len(),
        ),
        (ImportGroup::Nicknames, collections.nicknames.len()),
        (ImportGroup::Commands, collections.custom_commands.len()),
        (ImportGroup::ModButtons, collections.mod_buttons.len()),
    ] {
        outcome.imported[group.index()] = count;
    }

    StagedImport {
        collections,
        tabs,
        selected_tab,
        tab_mode: selection.tab_mode,
        outcome,
    }
}

/// Copies taken before an import writes anything.
#[derive(Clone, Debug, Default)]
pub struct ImportBackups {
    pub settings: Option<PathBuf>,
    pub tabs: Option<PathBuf>,
}

impl ImportBackups {
    /// File names for the result line.
    pub fn file_names(&self) -> Vec<String> {
        [&self.settings, &self.tabs]
            .into_iter()
            .flatten()
            .filter_map(|path| path.file_name())
            .map(|name| name.to_string_lossy().into_owned())
            .collect()
    }
}

/// Back up settings.json and tabs.json into the settings backups folder.
fn create_backups() -> Result<ImportBackups, String> {
    let settings = settings::SettingsManager::new()
        .create_import_backup()
        .map_err(|error| format!("Couldn't back up settings ({error})"))?;
    let tabs = backup_tabs_file().map_err(|error| format!("Couldn't back up tabs ({error})"))?;
    Ok(ImportBackups { settings, tabs })
}

fn backup_tabs_file() -> Result<Option<PathBuf>, String> {
    let tabs_file = crate::config::tabs_file_path().map_err(|error| error.to_string())?;
    if !tabs_file.exists() {
        return Ok(None);
    }
    let backup_dir = settings::settings_backups_dir_path();
    std::fs::create_dir_all(&backup_dir).map_err(|error| error.to_string())?;
    let backup = backup_dir.join(format!(
        "tabs-before-chatterino-import-{}.json",
        crate::utils::unix_now_ms()
    ));
    std::fs::copy(&tabs_file, &backup).map_err(|error| error.to_string())?;
    Ok(Some(backup))
}

/// The disk half of an import. Blocking: run it off the UI thread.
///
/// Checks the source files against the preview, flushes queued tab saves,
/// backs up settings.json and tabs.json, then writes the staged settings and
/// tabs through their save workers. Nothing in memory changes here, so a
/// failure leaves the running app untouched; a tab write that fails after the
/// settings write rewrites settings.json from the unchanged live settings.
pub fn write_import(
    plan: &ImportPlan,
    staged_settings: Option<AppSettings>,
    staged_tabs: Option<Arc<Vec<TabState>>>,
) -> Result<ImportBackups, ImportError> {
    if plan.sources_changed() {
        return Err(ImportError::SourcesChanged);
    }
    crate::config::flush_tab_saves(WRITE_TIMEOUT);
    let backups = create_backups().map_err(ImportError::Failed)?;

    let wrote_settings = staged_settings.is_some();
    if let Some(snapshot) = staged_settings {
        settings::save_snapshot_and_wait(snapshot, WRITE_TIMEOUT).map_err(|error| {
            ImportError::Failed(format!(
                "Couldn't save settings ({error}). Nothing was imported."
            ))
        })?;
    }

    if let Some(tabs) = staged_tabs
        && !crate::bench::ui_fixture::should_suppress_persistence()
        && let Err(error) = crate::config::save_tabs_and_wait(tabs, WRITE_TIMEOUT)
    {
        // tabs.json is written atomically, so a failed write left it intact.
        if wrote_settings {
            restore_settings_file(&backups);
        }
        return Err(ImportError::Failed(format!(
            "Couldn't save tabs ({error}). Nothing was imported."
        )));
    }
    Ok(backups)
}

/// Undo a settings.json write by saving the live settings, which never held
/// the import, falling back to the backup copy.
fn restore_settings_file(backups: &ImportBackups) {
    if settings::save_settings_and_wait(WRITE_TIMEOUT).is_ok() {
        return;
    }
    let restored = match &backups.settings {
        Some(backup) => std::fs::copy(backup, settings::settings_file_path())
            .map(|_| ())
            .map_err(|error| error.to_string()),
        None => Err("there was no earlier settings file to restore".to_string()),
    };
    if let Err(error) = restored {
        crate::log_persist!(
            warn,
            "Couldn't restore settings after a failed import: {}",
            error
        );
    }
}
