//! Read-only import of a Chatterino setup.
//!
//! Chatterino keeps its state as JSON under `<root>/Settings/`: `settings.json`
//! (settings nested by setting path, so `/highlighting/highlights` is
//! `{"highlighting": {"highlights": [...]}}`), `window-layout.json` (windows,
//! then tabs, then a `splits2` node tree) and `commands.json`. This module
//! reads those files with a size cap and turns them into an [`ImportPlan`]:
//! the entries Echo can bring over, grouped the way the import preview shows
//! them, plus every entry it cannot bring over with the reason.
//!
//! The raw text of `settings.json` includes the `accounts` block with OAuth
//! tokens. It is held only while the file is parsed, its `Debug` output is
//! redacted, and only the keys named here are deserialized, so `accounts` never
//! reaches a plan. Nothing in this module writes to the Chatterino folder.

use crate::channel_identity::{WATCHING_CHANNEL, WHISPERS_CHANNEL};
use crate::filter_expression::FilterExpression;
use crate::workspace::{SplitNode, SplitOrientation};
use regex::{Regex, RegexBuilder};
use serde::Deserialize;
use serde_json::Value;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use std::time::SystemTime;

/// Largest Chatterino file the importer reads.
pub const MAX_FILE_BYTES: u64 = 16 * 1024 * 1024;

pub const SETTINGS_FILE: &str = "settings.json";
pub const WINDOW_LAYOUT_FILE: &str = "window-layout.json";
pub const COMMANDS_FILE: &str = "commands.json";

/// Split trees deeper than this are cut off instead of recursing further.
const MAX_SPLIT_DEPTH: usize = 32;

/// Share given to a Chatterino split whose flex weight is 0. Chatterino draws
/// such a split at its minimum size, so it gets a small slice, not an equal one.
const MIN_FLEX: f64 = 0.05;

/// What the preview lists, in display order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ImportGroup {
    Tabs,
    Highlights,
    HighlightIgnoredUsers,
    IgnoredPhrases,
    Nicknames,
    Filters,
    Commands,
    ModButtons,
}

impl ImportGroup {
    pub const COUNT: usize = 8;

    pub const ALL: [ImportGroup; Self::COUNT] = [
        ImportGroup::Tabs,
        ImportGroup::Highlights,
        ImportGroup::HighlightIgnoredUsers,
        ImportGroup::IgnoredPhrases,
        ImportGroup::Nicknames,
        ImportGroup::Filters,
        ImportGroup::Commands,
        ImportGroup::ModButtons,
    ];

    pub const fn index(self) -> usize {
        self as usize
    }

    pub fn label(self) -> &'static str {
        match self {
            ImportGroup::Tabs => "Tabs and splits",
            ImportGroup::Highlights => "Highlights",
            ImportGroup::HighlightIgnoredUsers => "Users excluded from highlights",
            ImportGroup::IgnoredPhrases => "Ignored phrases",
            ImportGroup::Nicknames => "Nicknames",
            ImportGroup::Filters => "Filters",
            ImportGroup::Commands => "Custom commands",
            ImportGroup::ModButtons => "Moderation buttons",
        }
    }

    /// Stable fragment for control ids.
    pub fn slug(self) -> &'static str {
        match self {
            ImportGroup::Tabs => "tabs",
            ImportGroup::Highlights => "highlights",
            ImportGroup::HighlightIgnoredUsers => "highlight-ignored-users",
            ImportGroup::IgnoredPhrases => "ignored-phrases",
            ImportGroup::Nicknames => "nicknames",
            ImportGroup::Filters => "filters",
            ImportGroup::Commands => "commands",
            ImportGroup::ModButtons => "mod-buttons",
        }
    }

    /// Commands and moderation buttons run text against Twitch, so the user
    /// opts into them; everything else is preselected.
    pub fn checked_by_default(self) -> bool {
        !matches!(self, ImportGroup::Commands | ImportGroup::ModButtons)
    }

    /// "6 tabs", "1 nickname".
    pub fn count_phrase(self, count: usize) -> String {
        let (one, many) = match self {
            ImportGroup::Tabs => ("tab", "tabs"),
            ImportGroup::Highlights => ("highlight", "highlights"),
            ImportGroup::HighlightIgnoredUsers => (
                "user excluded from highlights",
                "users excluded from highlights",
            ),
            ImportGroup::IgnoredPhrases => ("ignored phrase", "ignored phrases"),
            ImportGroup::Nicknames => ("nickname", "nicknames"),
            ImportGroup::Filters => ("filter", "filters"),
            ImportGroup::Commands => ("command", "commands"),
            ImportGroup::ModButtons => ("moderation button", "moderation buttons"),
        };
        format!("{count} {}", if count == 1 { one } else { many })
    }
}

/// How imported tabs meet the tabs Echo already has.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TabImportMode {
    /// Add the imported tabs after the existing ones.
    #[default]
    Append,
    /// Close the existing tabs (they stay reopenable) and keep only the imported ones.
    Replace,
}

/// Which groups to import, and how to place tabs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ImportSelection {
    groups: [bool; ImportGroup::COUNT],
    pub tab_mode: TabImportMode,
}

impl Default for ImportSelection {
    fn default() -> Self {
        Self {
            groups: ImportGroup::ALL.map(ImportGroup::checked_by_default),
            tab_mode: TabImportMode::Append,
        }
    }
}

impl ImportSelection {
    pub fn includes(&self, group: ImportGroup) -> bool {
        self.groups[group.index()]
    }

    pub fn set(&mut self, group: ImportGroup, included: bool) {
        self.groups[group.index()] = included;
    }
}

/// One Chatterino tab converted to an Echo split layout.
#[derive(Clone, Debug)]
pub struct ImportedTab {
    pub title: Option<String>,
    /// The tab that was selected in Chatterino's main window.
    pub selected: bool,
    pub highlights_enabled: bool,
    /// The tab came from a Chatterino popup window.
    pub from_popup: bool,
    pub layout: SplitNode,
}

impl ImportedTab {
    /// Channel values of every leaf, in layout order.
    pub fn channels(&self) -> Vec<String> {
        self.layout.channels()
    }

    pub fn label(&self) -> String {
        tab_label(self.title.as_deref(), &self.channels())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HighlightKind {
    Phrase,
    User,
    Badge,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImportedHighlight {
    pub kind: HighlightKind,
    /// Phrase, user name, or Chatterino badge key (`moderator`, `subscriber/12`).
    pub pattern: String,
    pub regex: bool,
    pub case_sensitive: bool,
    pub show_in_mentions: bool,
    /// Flash the taskbar.
    pub alert: bool,
    pub play_sound: bool,
    /// Local custom sound file; `None` plays Echo's default sound.
    pub sound_path: Option<String>,
    /// `#AARRGGBB` or `#RRGGBB`.
    pub color: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImportedUserPattern {
    pub pattern: String,
    pub regex: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum IgnoreAction {
    /// Hide the whole message.
    Block,
    /// Replace the matched text (empty removes it).
    Replace(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImportedIgnorePhrase {
    pub pattern: String,
    pub regex: bool,
    pub case_sensitive: bool,
    pub action: IgnoreAction,
}

impl ImportedIgnorePhrase {
    /// Echo filter expression that hides messages this phrase blocks.
    ///
    /// Literal phrases become escaped regexes so case sensitivity survives:
    /// Echo's `contains` operator always ignores case.
    pub fn hide_filter_expression(&self) -> String {
        let pattern = if self.regex {
            self.pattern.clone()
        } else {
            regex::escape(&self.pattern)
        };
        let literal = if self.case_sensitive { "r" } else { "ri" };
        format!(
            "message.content match {literal}\"{}\"",
            quote_regex_literal(&pattern)
        )
    }

    /// Pattern and regex flag for an Echo Replace filter.
    ///
    /// Echo compiles Replace patterns case-insensitively and trims them, so a
    /// case-sensitive phrase is wrapped in `(?-i:...)` and a literal with
    /// surrounding spaces in `(?:...)`; both then become regexes.
    pub fn replace_pattern(&self) -> (String, bool) {
        let body = if self.regex {
            self.pattern.clone()
        } else {
            regex::escape(&self.pattern)
        };
        if self.case_sensitive {
            (format!("(?-i:{body})"), true)
        } else if self.regex {
            (body, true)
        } else if self.pattern.trim() != self.pattern {
            (format!("(?:{body})"), true)
        } else {
            (self.pattern.clone(), false)
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImportedNickname {
    pub name: String,
    pub replacement: String,
    pub regex: bool,
    pub case_sensitive: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImportedFilter {
    pub name: String,
    pub expression: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImportedCommand {
    /// Trigger, usually with its leading slash (`/hello`).
    pub name: String,
    pub text: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImportedModButton {
    pub label: String,
    pub command: String,
    pub destructive: bool,
}

/// An entry the import leaves behind, and why.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SkippedItem {
    /// `None` for a whole file that could not be read.
    pub group: Option<ImportGroup>,
    pub item: String,
    pub reason: String,
}

/// A difference that applies to the imported entries of a group.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImportNote {
    pub group: ImportGroup,
    pub text: String,
}

/// Everything found in one Chatterino settings folder.
#[derive(Clone, Debug, Default)]
pub struct ImportPlan {
    pub source_dir: PathBuf,
    pub tabs: Vec<ImportedTab>,
    pub highlights: Vec<ImportedHighlight>,
    pub highlight_ignored_users: Vec<ImportedUserPattern>,
    pub ignored_phrases: Vec<ImportedIgnorePhrase>,
    pub nicknames: Vec<ImportedNickname>,
    pub filters: Vec<ImportedFilter>,
    pub commands: Vec<ImportedCommand>,
    pub mod_buttons: Vec<ImportedModButton>,
    pub skipped: Vec<SkippedItem>,
    pub notes: Vec<ImportNote>,
    /// Size and modification time of the source files when they were read;
    /// `None` for a plan built from text.
    pub sources: Option<Vec<SourceStamp>>,
}

impl ImportPlan {
    /// Whether Chatterino's files changed since this plan was read.
    pub fn sources_changed(&self) -> bool {
        self.sources
            .as_ref()
            .is_some_and(|recorded| *recorded != source_stamps(&self.source_dir))
    }

    pub fn count(&self, group: ImportGroup) -> usize {
        match group {
            ImportGroup::Tabs => self.tabs.len(),
            ImportGroup::Highlights => self.highlights.len(),
            ImportGroup::HighlightIgnoredUsers => self.highlight_ignored_users.len(),
            ImportGroup::IgnoredPhrases => self.ignored_phrases.len(),
            ImportGroup::Nicknames => self.nicknames.len(),
            ImportGroup::Filters => self.filters.len(),
            ImportGroup::Commands => self.commands.len(),
            ImportGroup::ModButtons => self.mod_buttons.len(),
        }
    }

    /// Nothing importable was found.
    pub fn is_empty(&self) -> bool {
        ImportGroup::ALL.iter().all(|group| self.count(*group) == 0)
    }

    /// Splits across all imported tabs.
    pub fn split_count(&self) -> usize {
        self.tabs.iter().map(|tab| tab.layout.count_leaves()).sum()
    }

    pub fn skipped_in(&self, group: ImportGroup) -> impl Iterator<Item = &SkippedItem> {
        self.skipped
            .iter()
            .filter(move |item| item.group == Some(group))
    }

    pub fn notes_for(&self, group: ImportGroup) -> impl Iterator<Item = &ImportNote> {
        self.notes.iter().filter(move |note| note.group == group)
    }

    /// Problems with whole files (missing permissions, invalid JSON).
    pub fn file_problems(&self) -> impl Iterator<Item = &SkippedItem> {
        self.skipped.iter().filter(|item| item.group.is_none())
    }

    fn skip(&mut self, group: ImportGroup, item: impl Into<String>, reason: impl Into<String>) {
        self.skipped.push(SkippedItem {
            group: Some(group),
            item: item.into(),
            reason: reason.into(),
        });
    }

    fn note(&mut self, group: ImportGroup, text: impl Into<String>) {
        self.notes.push(ImportNote {
            group,
            text: text.into(),
        });
    }
}

/// What an import changed, for the result line.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ImportOutcome {
    pub imported: [usize; ImportGroup::COUNT],
    /// Entries Echo already had, per group.
    pub already_present: [usize; ImportGroup::COUNT],
    /// Entries Chatterino listed more than once, imported once, per group.
    pub repeated: [usize; ImportGroup::COUNT],
    pub skipped: Vec<SkippedItem>,
    /// Names of the backups written before the import, in Echo's
    /// settings-backups folder.
    pub backup_files: Vec<String>,
}

impl ImportOutcome {
    pub fn total_imported(&self) -> usize {
        self.imported.iter().sum()
    }

    /// "Imported 6 tabs and 23 highlights. Skipped 2: … 4 were already in Echo."
    pub fn summary(&self) -> String {
        let imported: Vec<String> = ImportGroup::ALL
            .iter()
            .filter(|group| self.imported[group.index()] > 0)
            .map(|group| group.count_phrase(self.imported[group.index()]))
            .collect();
        let mut summary = if imported.is_empty() {
            "Nothing new to import.".to_string()
        } else {
            format!("Imported {}.", join_list(&imported))
        };
        if !self.skipped.is_empty() {
            const SHOWN: usize = 3;
            let mut shown: Vec<String> = self
                .skipped
                .iter()
                .take(SHOWN)
                .map(|item| format!("{} ({})", item.item, item.reason))
                .collect();
            if self.skipped.len() > SHOWN {
                shown.push(format!("{} more", self.skipped.len() - SHOWN));
            }
            summary.push_str(&format!(
                " Skipped {}: {}.",
                self.skipped.len(),
                shown.join("; ")
            ));
        }
        let present: usize = self.already_present.iter().sum();
        if present > 0 {
            summary.push_str(&format!(
                " {present} {} already in Echo.",
                if present == 1 { "was" } else { "were" }
            ));
        }
        let repeated: usize = self.repeated.iter().sum();
        if repeated > 0 {
            summary.push_str(&format!(
                " {repeated} repeated {} in Chatterino {} imported once.",
                if repeated == 1 { "entry" } else { "entries" },
                if repeated == 1 { "was" } else { "were" }
            ));
        }
        if !self.backup_files.is_empty() {
            summary.push_str(&format!(
                " Backed up {} in the settings-backups folder.",
                join_list(&self.backup_files)
            ));
        }
        summary
    }
}

fn join_list(items: &[String]) -> String {
    match items {
        [] => String::new(),
        [one] => one.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

/// The platform whose Chatterino install locations to use.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Platform {
    Windows,
    MacOs,
    Linux,
}

impl Platform {
    pub fn current() -> Self {
        if cfg!(windows) {
            Platform::Windows
        } else if cfg!(target_os = "macos") {
            Platform::MacOs
        } else {
            Platform::Linux
        }
    }
}

/// Chatterino roots for an install, most likely first.
///
/// `data_dir` is the OS per-user application data directory (Qt's
/// `AppDataLocation` parent): `%APPDATA%` on Windows, `~/Library/Application
/// Support` on macOS and `$XDG_DATA_HOME` on Linux. Portable installs keep
/// their data next to the executable and have to be picked by hand.
pub fn default_roots(
    platform: Platform,
    data_dir: Option<&Path>,
    home_dir: Option<&Path>,
) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Some(data_dir) = data_dir {
        roots.push(match platform {
            // Chatterino 1 took `Chatterino`, so Chatterino 2 appends a 2.
            Platform::Windows => data_dir.join("Chatterino2"),
            Platform::MacOs | Platform::Linux => data_dir.join("chatterino"),
        });
    }
    if platform == Platform::Linux
        && let Some(home_dir) = home_dir
    {
        roots.push(home_dir.join(".var/app/com.chatterino.chatterino/data/chatterino"));
    }
    roots
}

/// The folder holding Chatterino's JSON files for `path`, which may be the
/// Chatterino root, its `Settings` folder, or a portable install folder.
pub fn settings_dir_for(path: &Path) -> Option<PathBuf> {
    let nested = path.join("Settings");
    if has_chatterino_files(&nested) {
        return Some(nested);
    }
    has_chatterino_files(path).then(|| path.to_path_buf())
}

/// The first candidate root that holds a Chatterino setup.
pub fn discover(candidates: &[PathBuf]) -> Option<PathBuf> {
    candidates
        .iter()
        .find_map(|candidate| settings_dir_for(candidate))
}

fn has_chatterino_files(dir: &Path) -> bool {
    [SETTINGS_FILE, WINDOW_LAYOUT_FILE]
        .iter()
        .any(|name| dir.join(name).is_file())
}

/// Result of reading one Chatterino file.
#[derive(Clone, PartialEq, Eq)]
pub enum SourceText {
    Missing,
    /// Raw file text. settings.json carries OAuth tokens, so `Debug` prints
    /// only its length.
    Text(String),
    /// The file exists but could not be used; the text completes "<file> …".
    Problem(String),
}

impl std::fmt::Debug for SourceText {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SourceText::Missing => f.write_str("Missing"),
            SourceText::Text(text) => write!(f, "Text(<{} bytes>)", text.len()),
            SourceText::Problem(reason) => f.debug_tuple("Problem").field(reason).finish(),
        }
    }
}

/// Size and modification time of one source file (`None` when missing).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceStamp {
    pub file: &'static str,
    pub len: Option<u64>,
    pub modified: Option<SystemTime>,
}

/// Stamps for the three source files in `settings_dir`.
pub fn source_stamps(settings_dir: &Path) -> Vec<SourceStamp> {
    [SETTINGS_FILE, WINDOW_LAYOUT_FILE, COMMANDS_FILE]
        .into_iter()
        .map(|file| {
            let metadata = std::fs::metadata(settings_dir.join(file)).ok();
            SourceStamp {
                file,
                len: metadata.as_ref().map(std::fs::Metadata::len),
                modified: metadata.and_then(|metadata| metadata.modified().ok()),
            }
        })
        .collect()
}

/// Read a file as UTF-8, refusing anything over `cap` bytes.
pub fn read_capped(path: &Path, cap: u64) -> SourceText {
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return SourceText::Missing,
        Err(error) => return SourceText::Problem(format!("couldn't be opened ({error})")),
    };
    let too_large = || SourceText::Problem(format!("is larger than {} MiB", cap / (1024 * 1024)));
    if file.metadata().is_ok_and(|metadata| metadata.len() > cap) {
        return too_large();
    }
    let mut bytes = Vec::new();
    if let Err(error) = file.take(cap.saturating_add(1)).read_to_end(&mut bytes) {
        return SourceText::Problem(format!("couldn't be read ({error})"));
    }
    if bytes.len() as u64 > cap {
        return too_large();
    }
    match String::from_utf8(bytes) {
        Ok(text) => SourceText::Text(text),
        Err(_) => SourceText::Problem("isn't UTF-8 text".to_string()),
    }
}

/// The three files an import reads.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceFiles {
    pub settings: SourceText,
    pub window_layout: SourceText,
    pub commands: SourceText,
    /// Taken just before reading, so a later change is always noticed.
    pub stamps: Option<Vec<SourceStamp>>,
}

impl SourceFiles {
    pub fn read(settings_dir: &Path) -> Self {
        Self {
            stamps: Some(source_stamps(settings_dir)),
            settings: read_capped(&settings_dir.join(SETTINGS_FILE), MAX_FILE_BYTES),
            window_layout: read_capped(&settings_dir.join(WINDOW_LAYOUT_FILE), MAX_FILE_BYTES),
            commands: read_capped(&settings_dir.join(COMMANDS_FILE), MAX_FILE_BYTES),
        }
    }

    pub fn from_text(
        settings: Option<&str>,
        window_layout: Option<&str>,
        commands: Option<&str>,
    ) -> Self {
        let text = |value: Option<&str>| {
            value.map_or(SourceText::Missing, |text| {
                SourceText::Text(text.to_string())
            })
        };
        Self {
            settings: text(settings),
            window_layout: text(window_layout),
            commands: text(commands),
            stamps: None,
        }
    }
}

/// Read and parse a Chatterino settings folder.
pub fn load_plan(settings_dir: &Path) -> ImportPlan {
    build_plan(settings_dir, &SourceFiles::read(settings_dir))
}

/// Parse already-read Chatterino files into a plan.
pub fn build_plan(source_dir: &Path, files: &SourceFiles) -> ImportPlan {
    let mut plan = ImportPlan {
        source_dir: source_dir.to_path_buf(),
        sources: files.stamps.clone(),
        ..ImportPlan::default()
    };
    if [&files.settings, &files.window_layout, &files.commands]
        .iter()
        .all(|file| matches!(file, SourceText::Missing))
    {
        plan.skipped.push(SkippedItem {
            group: None,
            item: "Chatterino folder".to_string(),
            reason: format!("has no {SETTINGS_FILE}, {WINDOW_LAYOUT_FILE} or {COMMANDS_FILE}"),
        });
        return plan;
    }

    if let Some(text) = usable_text(&mut plan, WINDOW_LAYOUT_FILE, &files.window_layout) {
        match serde_json::from_str::<RawWindowLayout>(text) {
            Ok(layout) => parse_windows(&mut plan, layout.windows.as_ref()),
            Err(error) => file_problem(&mut plan, WINDOW_LAYOUT_FILE, &error),
        }
    }
    if let Some(text) = usable_text(&mut plan, SETTINGS_FILE, &files.settings) {
        match serde_json::from_str::<RawSettings>(text) {
            Ok(settings) => parse_settings(&mut plan, &settings),
            Err(error) => file_problem(&mut plan, SETTINGS_FILE, &error),
        }
    }
    if let Some(text) = usable_text(&mut plan, COMMANDS_FILE, &files.commands) {
        match serde_json::from_str::<RawCommands>(text) {
            Ok(commands) => parse_commands(&mut plan, commands.commands.as_ref()),
            Err(error) => file_problem(&mut plan, COMMANDS_FILE, &error),
        }
    }
    plan
}

fn usable_text<'a>(
    plan: &mut ImportPlan,
    file_name: &str,
    source: &'a SourceText,
) -> Option<&'a str> {
    match source {
        SourceText::Missing => None,
        SourceText::Text(text) => Some(text.strip_prefix('\u{feff}').unwrap_or(text)),
        SourceText::Problem(reason) => {
            plan.skipped.push(SkippedItem {
                group: None,
                item: file_name.to_string(),
                reason: reason.clone(),
            });
            None
        }
    }
}

/// Report a file that did not parse. Only the position is kept so file
/// contents never reach the preview or a log.
fn file_problem(plan: &mut ImportPlan, file_name: &str, error: &serde_json::Error) {
    let reason = if error.is_data() {
        "isn't laid out the way Chatterino writes it".to_string()
    } else {
        format!(
            "isn't valid JSON (line {}, column {})",
            error.line(),
            error.column()
        )
    };
    plan.skipped.push(SkippedItem {
        group: None,
        item: file_name.to_string(),
        reason,
    });
}

// Serde skips every key not named below, so `accounts` (and its tokens) is
// never materialized.
#[derive(Deserialize, Default)]
#[serde(default)]
struct RawSettings {
    highlighting: Option<Value>,
    ignore: Option<Value>,
    nicknames: Option<Value>,
    filtering: Option<Value>,
    moderation: Option<Value>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawWindowLayout {
    windows: Option<Value>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawCommands {
    commands: Option<Value>,
}

/// The list stored at `key` under `parent`. A value of the wrong shape is
/// reported once and treated as empty.
fn entries<'a>(
    plan: &mut ImportPlan,
    parent: Option<&'a Value>,
    key: &str,
    group: ImportGroup,
    what: &str,
) -> &'a [Value] {
    match parent.and_then(|parent| parent.get(key)) {
        None | Some(Value::Null) => &[],
        Some(Value::Array(items)) => items,
        Some(_) => {
            plan.skip(group, what, "isn't a list");
            &[]
        }
    }
}

fn text<'a>(entry: &'a Value, key: &str) -> Option<&'a str> {
    entry.get(key).and_then(Value::as_str)
}

fn trimmed<'a>(entry: &'a Value, key: &str) -> &'a str {
    text(entry, key).map(str::trim).unwrap_or("")
}

fn flag(entry: &Value, key: &str, default: bool) -> bool {
    entry.get(key).and_then(Value::as_bool).unwrap_or(default)
}

fn malformed(plan: &mut ImportPlan, group: ImportGroup, what: &str, index: usize) {
    plan.skip(
        group,
        format!("{what} #{}", index + 1),
        "isn't in a format Echo understands",
    );
}

fn parse_settings(plan: &mut ImportPlan, settings: &RawSettings) {
    parse_highlights(plan, settings.highlighting.as_ref());
    parse_highlight_ignored_users(plan, settings.highlighting.as_ref());
    parse_ignored_phrases(plan, settings.ignore.as_ref());
    parse_nicknames(plan, settings.nicknames.as_ref());
    parse_filters(plan, settings.filtering.as_ref());
    parse_mod_buttons(plan, settings.moderation.as_ref());
}

fn parse_highlights(plan: &mut ImportPlan, highlighting: Option<&Value>) {
    let group = ImportGroup::Highlights;
    let mut unplayable_sounds = 0usize;
    for (key, kind, what) in [
        ("highlights", HighlightKind::Phrase, "Highlight phrase"),
        ("users", HighlightKind::User, "User highlight"),
        ("badges", HighlightKind::Badge, "Badge highlight"),
    ] {
        for (index, entry) in entries(plan, highlighting, key, group, what)
            .iter()
            .enumerate()
        {
            if !entry.is_object() {
                malformed(plan, group, what, index);
                continue;
            }
            let field = if kind == HighlightKind::Badge {
                "name"
            } else {
                "pattern"
            };
            let pattern = trimmed(entry, field);
            if pattern.is_empty() {
                plan.skip(group, format!("{what} #{}", index + 1), "is empty");
                continue;
            }
            let (regex, case_sensitive) = if kind == HighlightKind::Badge {
                (false, false)
            } else {
                (flag(entry, "regex", false), flag(entry, "case", false))
            };
            if regex && let Some(problem) = regex_problem(pattern, case_sensitive) {
                plan.skip(group, pattern, problem);
                continue;
            }
            let play_sound = flag(entry, "sound", false);
            let sound_path = match sound_path(text(entry, "soundUrl").unwrap_or("")) {
                Ok(path) => path,
                Err(()) => {
                    unplayable_sounds += usize::from(play_sound);
                    None
                }
            };
            plan.highlights.push(ImportedHighlight {
                kind,
                pattern: pattern.to_string(),
                regex,
                case_sensitive,
                // Chatterino's defaults differ for badges.
                show_in_mentions: flag(entry, "showInMentions", kind != HighlightKind::Badge),
                alert: flag(entry, "alert", true),
                play_sound,
                sound_path,
                color: color(text(entry, "color")),
            });
        }
    }
    if unplayable_sounds > 0 {
        plan.note(
            group,
            format!(
                "{unplayable_sounds} custom sound(s) aren't local files, so those highlights use \
                 Echo's default sound."
            ),
        );
    }
}

fn parse_highlight_ignored_users(plan: &mut ImportPlan, highlighting: Option<&Value>) {
    let group = ImportGroup::HighlightIgnoredUsers;
    let what = "Excluded user";
    for (index, entry) in entries(plan, highlighting, "blacklist", group, what)
        .iter()
        .enumerate()
    {
        if !entry.is_object() {
            malformed(plan, group, what, index);
            continue;
        }
        let pattern = trimmed(entry, "pattern");
        if pattern.is_empty() {
            plan.skip(group, format!("{what} #{}", index + 1), "is empty");
            continue;
        }
        let regex = flag(entry, "regex", false);
        if regex && let Some(problem) = regex_problem(pattern, false) {
            plan.skip(group, pattern, problem);
            continue;
        }
        plan.highlight_ignored_users.push(ImportedUserPattern {
            pattern: pattern.to_string(),
            regex,
        });
    }
}

fn parse_ignored_phrases(plan: &mut ImportPlan, ignore: Option<&Value>) {
    let group = ImportGroup::IgnoredPhrases;
    let what = "Ignored phrase";
    for (index, entry) in entries(plan, ignore, "phrases", group, what)
        .iter()
        .enumerate()
    {
        if !entry.is_object() {
            malformed(plan, group, what, index);
            continue;
        }
        // Chatterino keeps the pattern verbatim; only reject a blank one.
        let pattern = text(entry, "pattern").unwrap_or("");
        if pattern.trim().is_empty() {
            plan.skip(group, format!("{what} #{}", index + 1), "is empty");
            continue;
        }
        let regex = flag(entry, "regex", false);
        let case_sensitive = flag(entry, "caseSensitive", true);
        if regex && let Some(problem) = regex_problem(pattern, case_sensitive) {
            plan.skip(group, pattern, problem);
            continue;
        }
        let action = if flag(entry, "isBlock", false) {
            IgnoreAction::Block
        } else {
            let replacement = text(entry, "replaceWith").unwrap_or("");
            // Qt expands `\1` in replacements; Echo inserts the text as written.
            if qt_backreference_at(replacement).is_some() {
                plan.skip(
                    group,
                    pattern,
                    "uses capture-group replacement, which Echo doesn't support",
                );
                continue;
            }
            IgnoreAction::Replace(replacement.to_string())
        };
        let phrase = ImportedIgnorePhrase {
            pattern: pattern.to_string(),
            regex,
            case_sensitive,
            action,
        };
        match &phrase.action {
            IgnoreAction::Block => {
                if let Err(error) = FilterExpression::parse(&phrase.hide_filter_expression()) {
                    plan.skip(
                        group,
                        pattern,
                        format!("couldn't be turned into an Echo filter ({error})"),
                    );
                    continue;
                }
            }
            IgnoreAction::Replace(_) => {}
        }
        plan.ignored_phrases.push(phrase);
    }
}

fn parse_nicknames(plan: &mut ImportPlan, nicknames: Option<&Value>) {
    let group = ImportGroup::Nicknames;
    let what = "Nickname";
    let items: &[Value] = match nicknames {
        None | Some(Value::Null) => &[],
        Some(Value::Array(items)) => items,
        Some(_) => {
            plan.skip(group, "Nicknames", "isn't a list");
            &[]
        }
    };
    for (index, entry) in items.iter().enumerate() {
        if !entry.is_object() {
            malformed(plan, group, what, index);
            continue;
        }
        let name = trimmed(entry, "name");
        if name.is_empty() {
            plan.skip(group, format!("{what} #{}", index + 1), "has no user name");
            continue;
        }
        let regex = flag(entry, "isRegex", false);
        let case_sensitive = flag(entry, "isCaseSensitive", false);
        if regex && let Some(problem) = regex_problem(name, case_sensitive) {
            plan.skip(group, name, problem);
            continue;
        }
        let replacement = text(entry, "replace").unwrap_or("");
        plan.nicknames.push(ImportedNickname {
            name: name.to_string(),
            replacement: if regex {
                qt_replacement_to_rust(replacement)
            } else {
                replacement.to_string()
            },
            regex,
            case_sensitive,
        });
    }
}

fn parse_filters(plan: &mut ImportPlan, filtering: Option<&Value>) {
    let group = ImportGroup::Filters;
    let what = "Filter";
    for (index, entry) in entries(plan, filtering, "filters", group, what)
        .iter()
        .enumerate()
    {
        if !entry.is_object() {
            malformed(plan, group, what, index);
            continue;
        }
        let name = trimmed(entry, "name");
        let label = if name.is_empty() {
            format!("{what} #{}", index + 1)
        } else {
            name.to_string()
        };
        let expression = trimmed(entry, "filter");
        if expression.is_empty() {
            plan.skip(group, label, "has no expression");
            continue;
        }
        if let Err(error) = FilterExpression::parse(expression) {
            plan.skip(
                group,
                label,
                format!("isn't an expression Echo understands ({error})"),
            );
            continue;
        }
        plan.filters.push(ImportedFilter {
            name: name.to_string(),
            expression: expression.to_string(),
        });
    }
    if !plan.filters.is_empty() {
        plan.note(
            group,
            "Echo filters apply to every channel, so Chatterino's split filters arrive turned \
             off with the Allow action and without their names. Turn one on in Moderation & \
             filters to use it.",
        );
    }
}

fn parse_mod_buttons(plan: &mut ImportPlan, moderation: Option<&Value>) {
    let group = ImportGroup::ModButtons;
    let what = "Moderation button";
    for (index, entry) in entries(plan, moderation, "actions", group, what)
        .iter()
        .enumerate()
    {
        if !entry.is_object() {
            malformed(plan, group, what, index);
            continue;
        }
        let command = trimmed(entry, "pattern");
        if command.is_empty() {
            plan.skip(group, format!("{what} #{}", index + 1), "has no command");
            continue;
        }
        // Icons are not imported: Echo draws labels, and an icon is a file
        // URL that could point at a network share.
        plan.mod_buttons.push(ImportedModButton {
            label: mod_button_label(command),
            command: command.to_string(),
            destructive: command.starts_with("/ban "),
        });
    }
}

fn parse_commands(plan: &mut ImportPlan, commands: Option<&Value>) {
    let group = ImportGroup::Commands;
    let what = "Command";
    let mut context_menu = 0usize;
    let items: &[Value] = match commands {
        None | Some(Value::Null) => &[],
        Some(Value::Array(items)) => items,
        Some(_) => {
            plan.skip(group, "Commands", "isn't a list");
            &[]
        }
    };
    for (index, entry) in items.iter().enumerate() {
        if !entry.is_object() {
            malformed(plan, group, what, index);
            continue;
        }
        let name = trimmed(entry, "name");
        if name.is_empty() {
            plan.skip(group, format!("{what} #{}", index + 1), "has no trigger");
            continue;
        }
        let text = trimmed(entry, "func");
        if text.is_empty() {
            plan.skip(group, name, "has no text to send");
            continue;
        }
        if flag(entry, "showInMsgContextMenu", false) {
            context_menu += 1;
        }
        plan.commands.push(ImportedCommand {
            name: name.to_string(),
            text: text.to_string(),
        });
    }
    if context_menu > 0 {
        plan.note(
            group,
            format!(
                "Echo has no message context-menu commands, so {context_menu} command(s) shown \
                 there in Chatterino run from the input box only."
            ),
        );
    }
}

fn parse_windows(plan: &mut ImportPlan, windows: Option<&Value>) {
    let group = ImportGroup::Tabs;
    let windows: &[Value] = match windows {
        None | Some(Value::Null) => &[],
        Some(Value::Array(windows)) => windows,
        Some(_) => {
            plan.skip(group, "Windows", "isn't a list");
            &[]
        }
    };
    let mut main_seen = false;
    let mut selected_seen = false;
    let mut popup_tabs = 0usize;
    let mut tab_number = 0usize;
    let mut differences = SplitDifferences::default();
    for window in windows {
        // Chatterino demotes every main window after the first to a popup.
        let is_main = text(window, "type") == Some("main") && !main_seen;
        main_seen |= is_main;
        let tabs: &[Value] = window
            .get("tabs")
            .and_then(Value::as_array)
            .map_or(&[], Vec::as_slice);
        for tab in tabs {
            tab_number += 1;
            let Some(mut imported) = convert_tab(plan, tab, tab_number, !is_main, &mut differences)
            else {
                continue;
            };
            imported.selected &= is_main && !selected_seen;
            selected_seen |= imported.selected;
            popup_tabs += usize::from(imported.from_popup);
            plan.tabs.push(imported);
        }
    }
    if popup_tabs > 0 {
        plan.note(
            group,
            format!(
                "{popup_tabs} tab(s) from Chatterino popup windows are added as ordinary tabs."
            ),
        );
    }
    for (splits, text) in [
        (
            &differences.moderation_mode,
            "had moderation mode on. Echo doesn't keep it per split, so turn it on in the split.",
        ),
        (
            &differences.split_filters,
            "had split filters. Echo filters apply to every channel, so they weren't attached.",
        ),
        (
            &differences.spell_check,
            "had their own spell check setting, which Echo doesn't have.",
        ),
    ] {
        if !splits.is_empty() {
            plan.note(group, format!("{} {text}", split_list(splits)));
        }
    }
}

/// Per-split Chatterino settings Echo has no place for, by split label.
#[derive(Default)]
struct SplitDifferences {
    moderation_mode: Vec<String>,
    split_filters: Vec<String>,
    spell_check: Vec<String>,
}

/// "Splits #a (Main), #b (Main) and 3 more" for a note.
fn split_list(splits: &[String]) -> String {
    const SHOWN: usize = 3;
    let mut names: Vec<String> = splits.iter().take(SHOWN).cloned().collect();
    if splits.len() > SHOWN {
        names.push(format!("{} more", splits.len() - SHOWN));
    }
    let noun = if splits.len() == 1 { "Split" } else { "Splits" };
    format!("{noun} {}", join_list(&names))
}

fn convert_tab(
    plan: &mut ImportPlan,
    tab: &Value,
    tab_number: usize,
    from_popup: bool,
    differences: &mut SplitDifferences,
) -> Option<ImportedTab> {
    let group = ImportGroup::Tabs;
    if !tab.is_object() {
        malformed(plan, group, "Tab", tab_number - 1);
        return None;
    }
    let title = text(tab, "title")
        .map(str::trim)
        .filter(|title| !title.is_empty())
        .map(str::to_string);
    let fallback_label = title.clone().unwrap_or_else(|| format!("Tab {tab_number}"));
    let root = tab
        .get("splits2")
        .filter(|root| root.as_object().is_some_and(|root| !root.is_empty()));
    let Some(root) = root else {
        plan.skip(group, fallback_label, "is an empty tab");
        return None;
    };
    let mut walker = SplitWalker {
        tab: fallback_label.clone(),
        skipped: Vec::new(),
        differences,
    };
    let layout = walker.convert(root, 0).map(|(node, _, _)| node);
    plan.skipped.append(&mut walker.skipped);
    let Some(layout) = layout else {
        plan.skip(
            group,
            fallback_label,
            "has no Twitch channels Echo can open",
        );
        return None;
    };
    Some(ImportedTab {
        title,
        selected: flag(tab, "selected", false),
        highlights_enabled: flag(tab, "highlightsEnabled", true),
        from_popup,
        layout,
    })
}

struct SplitWalker<'a> {
    tab: String,
    skipped: Vec<SkippedItem>,
    differences: &'a mut SplitDifferences,
}

impl SplitWalker<'_> {
    fn skip(&mut self, item: impl std::fmt::Display, reason: impl Into<String>) {
        self.skipped.push(SkippedItem {
            group: Some(ImportGroup::Tabs),
            item: format!("{}: {item}", self.tab),
            reason: reason.into(),
        });
    }

    /// Convert one `splits2` node to an Echo node plus its Chatterino
    /// horizontal and vertical flex weights.
    fn convert(&mut self, node: &Value, depth: usize) -> Option<(SplitNode, f64, f64)> {
        if !node.is_object() {
            self.skip("Split", "isn't in a format Echo understands");
            return None;
        }
        let flex_h = flex(node.get("flexh"));
        let flex_v = flex(node.get("flexv"));
        match text(node, "type") {
            Some("split") => self.convert_split(node).map(|leaf| (leaf, flex_h, flex_v)),
            Some(kind @ ("horizontal" | "vertical")) => {
                if depth >= MAX_SPLIT_DEPTH {
                    self.skip("Nested splits", "are nested too deeply");
                    return None;
                }
                let mut children: Vec<(SplitNode, f64, f64)> = node
                    .get("items")
                    .and_then(Value::as_array)
                    .map_or(&[][..], Vec::as_slice)
                    .iter()
                    .filter_map(|item| self.convert(item, depth + 1))
                    .collect();
                match children.len() {
                    0 => None,
                    1 => children.pop().map(|(child, _, _)| (child, flex_h, flex_v)),
                    _ => {
                        // Chatterino's horizontal container lays children out
                        // side by side, which is Echo's vertical divider.
                        let side_by_side = kind == "horizontal";
                        let weights: Vec<f64> = children
                            .iter()
                            .map(|(_, h, v)| if side_by_side { *h } else { *v })
                            .collect();
                        let total: f64 = weights.iter().sum();
                        let ratios = weights
                            .iter()
                            .map(|weight| (weight / total) as f32)
                            .collect();
                        let orientation = if side_by_side {
                            SplitOrientation::Vertical
                        } else {
                            SplitOrientation::Horizontal
                        };
                        let children = children.into_iter().map(|(child, _, _)| child).collect();
                        Some((
                            SplitNode::split(orientation, children, ratios),
                            flex_h,
                            flex_v,
                        ))
                    }
                }
            }
            _ => {
                self.skip("Split", "is of a kind Echo doesn't know");
                None
            }
        }
    }

    fn convert_split(&mut self, node: &Value) -> Option<SplitNode> {
        let data = node.get("data").unwrap_or(&Value::Null);
        let kind = text(data, "type").unwrap_or("");
        let name = text(data, "channel")
            .or_else(|| text(data, "name"))
            .map(str::trim)
            .unwrap_or("");
        let channel = match kind {
            "twitch" => match normalize_twitch_login(name) {
                Some(login) => login,
                None => {
                    let label = if name.is_empty() { "Split" } else { name };
                    self.skip(label, "isn't a valid Twitch channel name");
                    return None;
                }
            },
            "whispers" => WHISPERS_CHANNEL.to_string(),
            "watching" => WATCHING_CHANNEL.to_string(),
            "mentions" => {
                self.skip(
                    "Mentions split",
                    "Echo shows mentions in its Activity panel instead of a split",
                );
                return None;
            }
            "live" => {
                self.skip(
                    "Live split",
                    "Echo has no live-channels split; a tab can show only live channels instead",
                );
                return None;
            }
            "automod" => {
                self.skip("AutoMod split", "Echo has no AutoMod split");
                return None;
            }
            "kick" => {
                self.skip(
                    format!("Non-Twitch split {name}"),
                    "Echo supports Twitch only",
                );
                return None;
            }
            "multi" => {
                self.skip("Merged split", "Echo has no merged multi-channel splits");
                return None;
            }
            _ => {
                self.skip("Split", "isn't a Twitch channel");
                return None;
            }
        };

        let label = format!("{} ({})", split_label(&channel), self.tab);
        if flag(node, "moderationMode", false) {
            self.differences.moderation_mode.push(label.clone());
        }
        if node
            .get("filters")
            .and_then(Value::as_array)
            .is_some_and(|filters| !filters.is_empty())
        {
            self.differences.split_filters.push(label.clone());
        }
        if node.get("checkSpelling").is_some_and(Value::is_boolean) {
            self.differences.spell_check.push(label);
        }
        Some(SplitNode::leaf(channel))
    }
}

/// Chatterino flex weights default to 1. Chatterino clamps them at 0 and
/// then draws the split at its minimum size, which [`MIN_FLEX`] stands in for.
fn flex(value: Option<&Value>) -> f64 {
    match value.and_then(Value::as_f64) {
        Some(weight) if weight.is_finite() => weight.max(MIN_FLEX),
        _ => 1.0,
    }
}

fn normalize_twitch_login(name: &str) -> Option<String> {
    let login = crate::channel_identity::normalize_channel_key(name.trim());
    let valid = (1..=25).contains(&login.len())
        && login
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_');
    valid.then_some(login)
}

fn split_label(channel: &str) -> String {
    if channel == WATCHING_CHANNEL {
        "Watching".to_string()
    } else if channel == WHISPERS_CHANNEL {
        "Whispers".to_string()
    } else {
        format!("#{channel}")
    }
}

fn tab_label(title: Option<&str>, channels: &[String]) -> String {
    if let Some(title) = title {
        return title.to_string();
    }
    match channels {
        [] => "Tab".to_string(),
        [one] => split_label(one),
        [first, rest @ ..] => format!("{} +{}", split_label(first), rest.len()),
    }
}

/// Sorted, lowercased channels of a layout, for spotting a tab Echo already has.
pub fn layout_channel_key(layout: &SplitNode) -> Vec<String> {
    let mut channels: Vec<String> = layout
        .channels()
        .into_iter()
        .filter(|channel| !crate::workspace::is_placeholder(channel))
        .map(|channel| crate::channel_identity::normalize_channel_key(&channel))
        .collect();
    channels.sort_unstable();
    channels
}

/// Why a Chatterino regex can't be used, or `None` when Rust `regex` accepts it.
pub fn regex_problem(pattern: &str, case_sensitive: bool) -> Option<String> {
    match RegexBuilder::new(pattern)
        .case_insensitive(!case_sensitive)
        .unicode(true)
        .build()
    {
        Ok(_) => None,
        Err(regex::Error::Syntax(message)) => {
            let lower = message.to_ascii_lowercase();
            Some(
                if lower.contains("look-around") || lower.contains("look-behind") {
                    "uses look-ahead or look-behind, which Echo's regex engine doesn't support"
                        .to_string()
                } else if lower.contains("backreference") {
                    "uses a backreference, which Echo's regex engine doesn't support".to_string()
                } else {
                    let detail = message
                        .lines()
                        .find_map(|line| line.trim().strip_prefix("error:"))
                        .map(str::trim)
                        .unwrap_or("invalid syntax");
                    format!("isn't a regex Echo can use ({detail})")
                },
            )
        }
        Err(regex::Error::CompiledTooBig(_)) => {
            Some("is too large for Echo's regex engine".to_string())
        }
        Err(_) => Some("isn't a regex Echo can use".to_string()),
    }
}

/// Byte length of a Qt backreference (`\1` to `\99`) at the start of `text`.
fn qt_backreference_len(text: &str) -> Option<usize> {
    let bytes = text.as_bytes();
    if bytes.first() != Some(&b'\\') || !bytes.get(1).is_some_and(u8::is_ascii_digit) {
        return None;
    }
    Some(if bytes.get(2).is_some_and(u8::is_ascii_digit) {
        3
    } else {
        2
    })
}

/// Byte offset of the first Qt backreference in a replacement, if any.
fn qt_backreference_at(text: &str) -> Option<usize> {
    text.char_indices()
        .map(|(index, _)| index)
        .find(|index| qt_backreference_len(&text[*index..]).is_some())
}

/// A Qt `QString::replace` replacement (`\1` backreferences, `$` literal) as a
/// Rust `Regex::replace_all` replacement (`${1}`, `$$`).
pub fn qt_replacement_to_rust(replacement: &str) -> String {
    let mut converted = String::with_capacity(replacement.len() + 4);
    let mut rest = replacement;
    while let Some(ch) = rest.chars().next() {
        if let Some(len) = qt_backreference_len(rest) {
            converted.push_str(&format!("${{{}}}", &rest[1..len]));
            rest = &rest[len..];
            continue;
        }
        if ch == '$' {
            converted.push_str("$$");
        } else {
            converted.push(ch);
        }
        rest = &rest[ch.len_utf8()..];
    }
    converted
}

/// Comparison key for moderation buttons: whitespace collapsed, a `.` command
/// prefix read as `/`, and a timeout duration counted in seconds, so
/// `/timeout {user.name} 10m` matches `/timeout {user.name} 600`.
pub fn mod_button_key(command: &str) -> String {
    let collapsed = command.split_whitespace().collect::<Vec<_>>().join(" ");
    let collapsed = match collapsed.strip_prefix('.') {
        Some(rest) => format!("/{rest}"),
        None => collapsed,
    };
    let Some(captures) = TIMEOUT_ACTION.captures(&collapsed) else {
        return collapsed;
    };
    let (Some(amount), Some(unit)) = (captures.get(1), captures.get(2)) else {
        return collapsed;
    };
    format!(
        "{}{}{}",
        &collapsed[..amount.start()],
        timeout_seconds(amount.as_str(), unit.as_str()),
        &collapsed[unit.end()..]
    )
}

fn timeout_seconds(amount: &str, unit: &str) -> u64 {
    let amount: u64 = amount.parse().unwrap_or(0);
    amount.saturating_mul(match unit {
        "m" => 60,
        "h" => 60 * 60,
        "d" => 24 * 60 * 60,
        "w" => 7 * 24 * 60 * 60,
        _ => 1,
    })
}

/// Escape unescaped quotes so a regex fits inside an `r"..."` filter literal.
/// Escape pairs pass through untouched, as the filter lexer keeps them.
fn quote_regex_literal(pattern: &str) -> String {
    let mut quoted = String::with_capacity(pattern.len() + 2);
    let mut chars = pattern.chars();
    while let Some(ch) = chars.next() {
        match ch {
            '\\' => {
                quoted.push('\\');
                if let Some(next) = chars.next() {
                    quoted.push(next);
                }
            }
            '"' => quoted.push_str("\\\""),
            other => quoted.push(other),
        }
    }
    quoted
}

/// A Chatterino sound URL as an Echo sound path. `Ok(None)` is the default
/// sound (no URL, or a built-in `qrc:` sound); `Err` is a remote URL Echo
/// can't play.
fn sound_path(url: &str) -> Result<Option<String>, ()> {
    let url = url.trim();
    if url.is_empty() || url.starts_with("qrc:") {
        return Ok(None);
    }
    file_url_to_path(url).map(Some).ok_or(())
}

/// `file:///C:/a%20b.wav` to `C:/a b.wav`; `None` for any other scheme.
fn file_url_to_path(url: &str) -> Option<String> {
    let rest = url.strip_prefix("file://")?;
    let decoded = percent_decode(rest)?;
    let path = if let Some(local) = decoded.strip_prefix('/') {
        let bytes = local.as_bytes();
        if bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
            local.to_string()
        } else {
            decoded
        }
    } else {
        // `file://server/share/x` names a network host.
        format!("//{decoded}")
    };
    (!path.trim_matches('/').is_empty()).then_some(path)
}

fn percent_decode(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let hex = text.get(index + 1..index + 3)?;
            decoded.push(u8::from_str_radix(hex, 16).ok()?);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(decoded).ok()
}

/// Keep `#RRGGBB` and `#AARRGGBB` colors; drop anything else.
fn color(value: Option<&str>) -> Option<String> {
    let value = value?.trim();
    let digits = value.strip_prefix('#')?;
    (matches!(digits.len(), 6 | 8) && digits.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .then(|| value.to_string())
}

static TIMEOUT_ACTION: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[./]timeout.* (\d+)([mhdw]?)").expect("valid timeout regex"));

/// Button label the way Chatterino derives it from the command.
fn mod_button_label(command: &str) -> String {
    if let Some(captures) = TIMEOUT_ACTION.captures(command) {
        const MINUTE: u64 = 60;
        const HOUR: u64 = 60 * MINUTE;
        const DAY: u64 = 24 * HOUR;
        const WEEK: u64 = 7 * DAY;
        let seconds = timeout_seconds(&captures[1], &captures[2]);
        return if seconds < MINUTE {
            format!("{seconds}s")
        } else if seconds < HOUR {
            format!("{}m", seconds / MINUTE)
        } else if seconds < DAY {
            format!("{}h", seconds / HOUR)
        } else if seconds < WEEK {
            format!("{}d", seconds / DAY)
        } else if seconds > 2 * WEEK {
            ">2w".to_string()
        } else {
            format!("{}w", seconds / WEEK)
        };
    }
    if command.starts_with("/ban ") {
        return "Ban".to_string();
    }
    if command.starts_with("/delete ") {
        return "Delete".to_string();
    }
    let word: String = command
        .split_whitespace()
        .next()
        .unwrap_or(command)
        .chars()
        .filter(|ch| !matches!(ch, '!' | '/' | '.'))
        .collect();
    if word.is_empty() {
        command.to_string()
    } else {
        word
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::filter_expression::FilterContext;

    fn leaf_channel(node: &SplitNode) -> &str {
        match node {
            SplitNode::Leaf { channel, .. } => channel,
            SplitNode::Split { .. } => panic!("expected a leaf"),
        }
    }

    fn split_parts(node: &SplitNode) -> (&SplitOrientation, &[SplitNode], &[f32]) {
        match node {
            SplitNode::Split {
                orientation,
                children,
                ratios,
                ..
            } => (orientation, children, ratios),
            SplitNode::Leaf { .. } => panic!("expected a split"),
        }
    }

    fn plan(settings: Option<&str>, layout: Option<&str>, commands: Option<&str>) -> ImportPlan {
        build_plan(
            Path::new("Settings"),
            &SourceFiles::from_text(settings, layout, commands),
        )
    }

    fn skipped_reason<'a>(plan: &'a ImportPlan, item: &str) -> &'a str {
        plan.skipped
            .iter()
            .find(|skipped| skipped.item == item)
            .map(|skipped| skipped.reason.as_str())
            .unwrap_or_else(|| panic!("{item} not skipped: {:?}", plan.skipped))
    }

    /// Shaped like `WindowManager::save` output: a main window with a nested
    /// split tree and a popup window.
    const WINDOW_LAYOUT: &str = r#"{
        "windows": [
            {
                "type": "main",
                "state": "maximized",
                "x": 10, "y": 10, "width": 1200, "height": 800,
                "emotePopup": {"x": 0, "y": 0, "width": 300, "height": 500},
                "tabs": [
                    {
                        "title": "Main",
                        "highlightsEnabled": false,
                        "splits2": {
                            "type": "horizontal",
                            "items": [
                                {
                                    "type": "split",
                                    "moderationMode": true,
                                    "data": {"type": "twitch", "name": "Forsen"},
                                    "filters": ["0b0c6a4e-8d2b-4a3c-9b1e-5f6a7b8c9d0e"],
                                    "flexh": 3.0, "flexv": 1.0
                                },
                                {
                                    "type": "vertical",
                                    "items": [
                                        {"type": "split", "moderationMode": false, "data": {"type": "twitch", "name": "alice"}, "filters": [], "flexh": 1.0, "flexv": 1.0},
                                        {"type": "split", "moderationMode": false, "data": {"type": "whispers"}, "filters": [], "flexh": 1.0, "flexv": 3.0},
                                        {"type": "split", "moderationMode": false, "data": {"type": "mentions"}, "filters": [], "flexh": 1.0, "flexv": 1.0}
                                    ],
                                    "flexh": 1.0, "flexv": 1.0
                                }
                            ],
                            "flexh": 1.0, "flexv": 1.0
                        }
                    },
                    {
                        "selected": true,
                        "highlightsEnabled": true,
                        "splits2": {"type": "split", "moderationMode": false, "data": {"type": "watching"}, "filters": [], "flexh": 1.0, "flexv": 1.0}
                    },
                    {
                        "highlightsEnabled": true,
                        "splits2": {"type": "split", "data": {"type": "live"}, "filters": [], "flexh": 1.0, "flexv": 1.0}
                    },
                    {"highlightsEnabled": true, "splits2": {}}
                ]
            },
            {
                "type": "popup",
                "x": 50, "y": 50, "width": 400, "height": 600,
                "tabs": [
                    {
                        "selected": true,
                        "highlightsEnabled": true,
                        "splits2": {
                            "type": "vertical",
                            "items": [
                                {"type": "split", "data": {"type": "twitch", "name": "xqc"}, "filters": [], "flexh": 1.0, "flexv": 1.0},
                                {"type": "split", "data": {"type": "kick", "name": "someone", "roomID": 1, "userID": 2, "channelID": 3}, "filters": [], "flexh": 1.0, "flexv": 1.0},
                                {"type": "split", "data": {"type": "automod"}, "filters": [], "flexh": 1.0, "flexv": 1.0}
                            ],
                            "flexh": 1.0, "flexv": 1.0
                        }
                    }
                ]
            }
        ]
    }"#;

    #[test]
    fn chatterino_import_converts_nested_split_tree_with_flex_ratios() {
        let plan = plan(None, Some(WINDOW_LAYOUT), None);
        let main = &plan.tabs[0];
        assert_eq!(main.title.as_deref(), Some("Main"));
        assert!(!main.highlights_enabled);
        assert!(!main.selected);

        let (orientation, children, ratios) = split_parts(&main.layout);
        assert_eq!(*orientation, SplitOrientation::Vertical);
        assert_eq!(leaf_channel(&children[0]), "forsen");
        assert!((ratios[0] - 0.75).abs() < 1e-6);
        assert!((ratios[1] - 0.25).abs() < 1e-6);

        // The mentions split drops out of the stacked column; the remaining
        // two keep their vertical flex weights.
        let (inner_orientation, inner, inner_ratios) = split_parts(&children[1]);
        assert_eq!(*inner_orientation, SplitOrientation::Horizontal);
        assert_eq!(leaf_channel(&inner[0]), "alice");
        assert_eq!(leaf_channel(&inner[1]), WHISPERS_CHANNEL);
        assert!((inner_ratios[0] - 0.25).abs() < 1e-6);
        assert!((inner_ratios[1] - 0.75).abs() < 1e-6);

        let watching = &plan.tabs[1];
        assert!(watching.selected);
        assert_eq!(leaf_channel(&watching.layout), WATCHING_CHANNEL);
        assert_eq!(plan.split_count(), 5);
    }

    #[test]
    fn chatterino_import_reports_special_splits_and_split_settings() {
        let plan = plan(None, Some(WINDOW_LAYOUT), None);
        assert!(skipped_reason(&plan, "Main: Mentions split").contains("Activity"));
        let notes: Vec<_> = plan
            .notes_for(ImportGroup::Tabs)
            .map(|note| note.text.as_str())
            .collect();
        assert!(notes.contains(
            &"Split #forsen (Main) had moderation mode on. Echo doesn't keep it per split, so \
              turn it on in the split."
        ));
        assert!(notes.contains(
            &"Split #forsen (Main) had split filters. Echo filters apply to every channel, so \
              they weren't attached."
        ));
        assert!(
            !plan
                .skipped
                .iter()
                .any(|item| item.item.contains("#forsen")),
            "per-split differences are notes, not skipped entries"
        );
        assert!(skipped_reason(&plan, "Tab 3: Live split").contains("live"));
        assert_eq!(
            skipped_reason(&plan, "Tab 3"),
            "has no Twitch channels Echo can open"
        );
        assert_eq!(skipped_reason(&plan, "Tab 4"), "is an empty tab");
        assert_eq!(
            skipped_reason(&plan, "Tab 5: Non-Twitch split someone"),
            "Echo supports Twitch only"
        );
        assert!(skipped_reason(&plan, "Tab 5: AutoMod split").contains("AutoMod"));
        assert!(
            plan.skipped
                .iter()
                .all(|item| item.group == Some(ImportGroup::Tabs))
        );
    }

    #[test]
    fn chatterino_import_adds_popup_tabs_unselected_with_a_note() {
        let plan = plan(None, Some(WINDOW_LAYOUT), None);
        assert_eq!(plan.tabs.len(), 3);
        let popup = &plan.tabs[2];
        assert!(popup.from_popup);
        assert!(!popup.selected, "only the main window's selection counts");
        assert_eq!(leaf_channel(&popup.layout), "xqc");
        assert_eq!(plan.tabs.iter().filter(|tab| tab.selected).count(), 1);
        assert!(
            plan.notes_for(ImportGroup::Tabs)
                .any(|note| note.text.starts_with("1 tab(s) from Chatterino popup"))
        );
    }

    /// Shaped like a nested `settings.json` document.
    const SETTINGS: &str = r##"{
        "accounts": {
            "current": "testaccount",
            "uid123456": {
                "username": "testaccount",
                "userID": "123456",
                "clientID": "client-id-value",
                "oauthToken": "SECRET-OAUTH-TOKEN-VALUE"
            }
        },
        "highlighting": {
            "selfHighlight": {"nameIsHighlightKeyword": true},
            "highlights": [
                {"pattern": "pog", "showInMentions": true, "alert": false, "sound": true, "regex": false, "case": false, "soundUrl": "file:///C:/Sounds/My%20Ping.wav", "color": "#7f7f3f49"},
                {"pattern": "(?<=@)echo", "showInMentions": true, "alert": true, "sound": false, "regex": true, "case": false, "soundUrl": "", "color": "#7f7f3f49"},
                {"pattern": "(a)\\1", "regex": true},
                {"pattern": "   "},
                "not an object"
            ],
            "users": [
                {"pattern": "alice", "showInMentions": false, "alert": true, "sound": true, "regex": false, "case": false, "soundUrl": "https://example.com/ping.wav", "color": "nonsense"},
                {"pattern": "quiet", "sound": false, "soundUrl": "https://example.com/unused.wav"}
            ],
            "badges": [
                {"name": "moderator", "displayName": "Moderator", "showInMentions": false, "alert": true, "sound": false, "soundUrl": "qrc:/sounds/ping2.wav", "color": "#7f7f3f49"}
            ],
            "blacklist": [
                {"pattern": "nightbot", "regex": false},
                {"pattern": "^bot_.*$", "regex": true}
            ]
        },
        "ignore": {
            "enableTwitchBlockedUsers": true,
            "phrases": [
                {"pattern": "badword", "regex": false, "isBlock": false, "replaceWith": "***", "caseSensitive": false},
                {"pattern": "Spam \"Link\"", "regex": false, "isBlock": true, "replaceWith": "", "caseSensitive": true},
                {"pattern": "b[a]+d", "regex": true, "isBlock": true, "replaceWith": "", "caseSensitive": false},
                {"pattern": "(?<!x)y", "regex": true, "isBlock": true},
                {"pattern": "(\\w+)bad", "regex": true, "isBlock": false, "replaceWith": "\\1***", "caseSensitive": false},
                {"pattern": "Cost", "regex": false, "isBlock": false, "replaceWith": "$5", "caseSensitive": true},
                {"pattern": " spaced ", "regex": false, "isBlock": false, "replaceWith": "", "caseSensitive": false}
            ]
        },
        "nicknames": [
            {"name": "alice", "replace": "ali", "isRegex": false, "isCaseSensitive": false},
            {"name": "^(\\w+)_tv$", "replace": "\\1", "isRegex": true, "isCaseSensitive": false},
            {"name": "^cash_(\\d+)$", "replace": "$\\1 cash", "isRegex": true},
            {"name": "money", "replace": "$$$", "isRegex": false}
        ],
        "filtering": {
            "filters": [
                {"name": "Subs only", "filter": "author.subbed", "id": "0b0c6a4e-8d2b-4a3c-9b1e-5f6a7b8c9d0e"},
                {"name": "Broken", "filter": "author.subbed &&", "id": "1b0c6a4e-8d2b-4a3c-9b1e-5f6a7b8c9d0e"}
            ]
        },
        "moderation": {
            "actions": [
                {"pattern": "/timeout {user.name} 300", "icon": ""},
                {"pattern": "/timeout {user.name} 2h", "icon": ""},
                {"pattern": "/ban {user.name}", "icon": "file:///home/me/ban.png"},
                {"pattern": "/unban {user.name}", "icon": "https://example.com/icon.png"}
            ]
        }
    }"##;

    #[test]
    fn chatterino_import_never_keeps_account_data() {
        let plan = plan(Some(SETTINGS), None, None);
        let dump = format!("{plan:?}");
        assert!(!dump.contains("SECRET-OAUTH-TOKEN-VALUE"));
        assert!(!dump.contains("client-id-value"));
        assert!(!dump.contains("testaccount"));
        let files = SourceFiles::from_text(Some(SETTINGS), None, None);
        let raw = format!("{files:?}");
        assert!(!raw.contains("SECRET-OAUTH-TOKEN-VALUE"));
        assert!(raw.contains("bytes>"));
    }

    #[test]
    fn chatterino_import_maps_highlights_and_skips_pcre_only_regexes() {
        let plan = plan(Some(SETTINGS), None, None);
        let kinds: Vec<_> = plan.highlights.iter().map(|h| h.kind).collect();
        assert_eq!(
            kinds,
            [
                HighlightKind::Phrase,
                HighlightKind::User,
                HighlightKind::User,
                HighlightKind::Badge
            ]
        );
        let phrase = &plan.highlights[0];
        assert_eq!(phrase.pattern, "pog");
        assert!(!phrase.alert);
        assert!(phrase.play_sound);
        assert_eq!(phrase.sound_path.as_deref(), Some("C:/Sounds/My Ping.wav"));
        assert_eq!(phrase.color.as_deref(), Some("#7f7f3f49"));

        let user = &plan.highlights[1];
        assert_eq!(user.sound_path, None);
        assert_eq!(user.color, None);
        let badge = &plan.highlights[3];
        assert_eq!(badge.pattern, "moderator");
        assert!(!badge.show_in_mentions);
        assert_eq!(badge.sound_path, None);

        assert!(skipped_reason(&plan, "(?<=@)echo").contains("look-behind"));
        assert!(skipped_reason(&plan, "(a)\\1").contains("backreference"));
        assert_eq!(skipped_reason(&plan, "Highlight phrase #4"), "is empty");
        assert_eq!(
            skipped_reason(&plan, "Highlight phrase #5"),
            "isn't in a format Echo understands"
        );
        assert!(
            plan.notes_for(ImportGroup::Highlights)
                .any(|note| note.text.starts_with("1 custom sound"))
        );
        assert_eq!(plan.highlight_ignored_users.len(), 2);
        assert!(plan.highlight_ignored_users[1].regex);
    }

    #[test]
    fn chatterino_import_maps_block_and_replace_ignore_phrases() {
        let plan = plan(Some(SETTINGS), None, None);
        assert_eq!(plan.ignored_phrases.len(), 5);
        assert_eq!(
            plan.ignored_phrases[0].action,
            IgnoreAction::Replace("***".to_string())
        );
        assert_eq!(
            plan.ignored_phrases[0].replace_pattern(),
            ("badword".to_string(), false)
        );
        // Chatterino's default is case-sensitive; Echo's Replace ignores case,
        // so the pattern pins case itself. `$` stays literal.
        let cost = &plan.ignored_phrases[3];
        assert_eq!(cost.action, IgnoreAction::Replace("$5".to_string()));
        assert_eq!(cost.replace_pattern(), ("(?-i:Cost)".to_string(), true));
        let pinned = RegexBuilder::new(&cost.replace_pattern().0)
            .case_insensitive(true)
            .build()
            .unwrap();
        assert!(pinned.is_match("Cost") && !pinned.is_match("cost"));
        assert_eq!(
            plan.ignored_phrases[4].replace_pattern(),
            ("(?: spaced )".to_string(), true)
        );
        assert_eq!(
            skipped_reason(&plan, "(\\w+)bad"),
            "uses capture-group replacement, which Echo doesn't support"
        );
        assert_eq!(plan.notes_for(ImportGroup::IgnoredPhrases).count(), 0);

        let quoted = &plan.ignored_phrases[1];
        assert_eq!(quoted.action, IgnoreAction::Block);
        let expression = FilterExpression::parse(&quoted.hide_filter_expression()).unwrap();
        let matches = |content: &str| {
            expression
                .evaluate(&FilterContext {
                    content: content.to_string(),
                    ..FilterContext::default()
                })
                .unwrap()
                .matched
        };
        assert!(matches("buy Spam \"Link\" now"));
        assert!(
            !matches("buy spam \"link\" now"),
            "block phrases stay case-sensitive"
        );
        assert!(!matches("Spam Link"));

        let regex = &plan.ignored_phrases[2];
        let expression = FilterExpression::parse(&regex.hide_filter_expression()).unwrap();
        assert!(
            expression
                .evaluate(&FilterContext {
                    content: "so BAAAD".to_string(),
                    ..FilterContext::default()
                })
                .unwrap()
                .matched
        );
        assert!(skipped_reason(&plan, "(?<!x)y").contains("look-behind"));
    }

    #[test]
    fn chatterino_import_maps_nicknames_filters_and_mod_buttons() {
        let plan = plan(Some(SETTINGS), None, None);
        assert_eq!(plan.nicknames.len(), 4);
        assert_eq!(plan.nicknames[0].replacement, "ali");
        assert!(plan.nicknames[1].regex);
        // Qt's `\1` becomes Rust's `${1}`, and a literal `$` is escaped.
        assert_eq!(plan.nicknames[1].replacement, "${1}");
        let rule = regex::Regex::new(&plan.nicknames[1].name).unwrap();
        assert_eq!(
            rule.replace_all("alice_tv", plan.nicknames[1].replacement.as_str()),
            "alice"
        );
        assert_eq!(plan.nicknames[2].replacement, "$$${1} cash");
        let rule = regex::Regex::new(&plan.nicknames[2].name).unwrap();
        assert_eq!(
            rule.replace_all("cash_5", plan.nicknames[2].replacement.as_str()),
            "$5 cash"
        );
        assert_eq!(
            plan.nicknames[3].replacement, "$$$",
            "plain nicknames are inserted as written"
        );

        assert_eq!(
            plan.filters,
            [ImportedFilter {
                name: "Subs only".to_string(),
                expression: "author.subbed".to_string()
            }]
        );
        assert!(skipped_reason(&plan, "Broken").starts_with("isn't an expression"));
        assert_eq!(plan.notes_for(ImportGroup::Filters).count(), 1);

        let labels: Vec<_> = plan.mod_buttons.iter().map(|b| b.label.as_str()).collect();
        assert_eq!(labels, ["5m", "2h", "Ban", "unban"]);
        assert!(plan.mod_buttons[2].destructive);
        assert_eq!(plan.notes_for(ImportGroup::ModButtons).count(), 0);
    }

    #[test]
    fn chatterino_import_parses_commands_json() {
        let plan = plan(
            None,
            None,
            Some(
                r#"{"commands": [
                    {"name": "/hello", "func": "Hello {1}!", "showInMsgContextMenu": true},
                    {"name": "/empty", "func": "  "},
                    {"name": "", "func": "x"},
                    {"name": "!dance", "func": "/me dances", "showInMsgContextMenu": false}
                ]}"#,
            ),
        );
        let names: Vec<_> = plan.commands.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["/hello", "!dance"]);
        assert_eq!(plan.commands[0].text, "Hello {1}!");
        assert_eq!(skipped_reason(&plan, "/empty"), "has no text to send");
        assert_eq!(skipped_reason(&plan, "Command #3"), "has no trigger");
        assert_eq!(plan.notes_for(ImportGroup::Commands).count(), 1);
    }

    #[test]
    fn chatterino_import_tolerates_malformed_and_partial_files() {
        let truncated_layout = &WINDOW_LAYOUT[..WINDOW_LAYOUT.len() / 2];
        let plan = plan(
            Some(
                r#"{"highlighting": {"highlights": "oops", "users": [{"pattern": 5}]}, "nicknames": [{"name": "kept", "replace": "k"}], "unknownFutureKey": {"x": 1}}"#,
            ),
            Some(truncated_layout),
            Some(r#""just text""#),
        );
        assert!(plan.tabs.is_empty());
        assert!(skipped_reason(&plan, WINDOW_LAYOUT_FILE).starts_with("isn't valid JSON (line"));
        assert_eq!(
            skipped_reason(&plan, COMMANDS_FILE),
            "isn't laid out the way Chatterino writes it"
        );
        assert_eq!(skipped_reason(&plan, "Highlight phrase"), "isn't a list");
        assert_eq!(skipped_reason(&plan, "User highlight #1"), "is empty");
        assert_eq!(plan.nicknames.len(), 1, "valid groups still import");
        assert_eq!(plan.file_problems().count(), 2);

        let empty = self::plan(None, None, None);
        assert!(empty.is_empty());
        assert_eq!(empty.file_problems().count(), 1);
    }

    #[test]
    fn chatterino_import_reads_with_size_cap_and_finds_the_settings_folder() {
        let root = std::env::temp_dir().join(format!(
            "echo-chatterino-import-test-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let settings_dir = root.join("Settings");
        std::fs::create_dir_all(&settings_dir).unwrap();
        std::fs::write(settings_dir.join(WINDOW_LAYOUT_FILE), WINDOW_LAYOUT).unwrap();
        std::fs::write(settings_dir.join(SETTINGS_FILE), "\u{feff}{}").unwrap();

        assert_eq!(settings_dir_for(&root), Some(settings_dir.clone()));
        assert_eq!(settings_dir_for(&settings_dir), Some(settings_dir.clone()));
        assert_eq!(
            discover(&[root.join("missing"), root.clone()]),
            Some(settings_dir.clone())
        );
        assert_eq!(discover(&[root.join("missing")]), None);

        let layout = settings_dir.join(WINDOW_LAYOUT_FILE);
        assert_eq!(
            read_capped(&layout, 64),
            SourceText::Problem("is larger than 0 MiB".to_string())
        );
        assert!(matches!(
            read_capped(&layout, MAX_FILE_BYTES),
            SourceText::Text(_)
        ));
        assert_eq!(
            read_capped(&settings_dir.join(COMMANDS_FILE), MAX_FILE_BYTES),
            SourceText::Missing
        );

        let plan = load_plan(&settings_dir);
        assert_eq!(plan.tabs.len(), 3);
        assert_eq!(plan.file_problems().count(), 0, "a BOM is tolerated");
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn chatterino_import_default_roots_follow_qt_app_data_locations() {
        let data = Path::new("DATA");
        let home = Path::new("HOME");
        assert_eq!(
            default_roots(Platform::Windows, Some(data), Some(home)),
            [data.join("Chatterino2")]
        );
        assert_eq!(
            default_roots(Platform::MacOs, Some(data), Some(home)),
            [data.join("chatterino")]
        );
        assert_eq!(
            default_roots(Platform::Linux, Some(data), Some(home)),
            [
                data.join("chatterino"),
                home.join(".var/app/com.chatterino.chatterino/data/chatterino")
            ]
        );
        assert!(default_roots(Platform::Windows, None, None).is_empty());
    }

    #[test]
    fn chatterino_import_converts_file_urls() {
        assert_eq!(
            file_url_to_path("file:///C:/a%20b.wav").as_deref(),
            Some("C:/a b.wav")
        );
        assert_eq!(
            file_url_to_path("file:///home/me/x.wav").as_deref(),
            Some("/home/me/x.wav")
        );
        assert_eq!(
            file_url_to_path("file://server/share/x.wav").as_deref(),
            Some("//server/share/x.wav")
        );
        assert_eq!(file_url_to_path("https://example.com/x.wav"), None);
        assert_eq!(file_url_to_path("file:///bad%zz"), None);
        assert_eq!(sound_path("qrc:/sounds/ping2.wav"), Ok(None));
        assert_eq!(sound_path("https://example.com/x.wav"), Err(()));
    }

    #[test]
    fn chatterino_import_derives_mod_button_labels_from_commands() {
        assert_eq!(mod_button_label("/timeout {user.name} 1"), "1s");
        assert_eq!(mod_button_label(".timeout {user.name} 600"), "10m");
        assert_eq!(mod_button_label("/timeout {user.name} 1d"), "1d");
        assert_eq!(mod_button_label("/timeout {user.name} 2w"), "2w");
        assert_eq!(mod_button_label("/timeout {user.name} 3w"), ">2w");
        assert_eq!(mod_button_label("/delete {msg.id}"), "Delete");
        assert_eq!(mod_button_label("!vanish"), "vanish");
    }

    #[test]
    fn chatterino_import_selection_defaults_leave_commands_and_buttons_off() {
        let selection = ImportSelection::default();
        for group in ImportGroup::ALL {
            assert_eq!(
                selection.includes(group),
                !matches!(group, ImportGroup::Commands | ImportGroup::ModButtons)
            );
        }
        assert_eq!(selection.tab_mode, TabImportMode::Append);
    }

    #[test]
    fn chatterino_import_outcome_summarizes_counts_skips_and_duplicates() {
        let mut outcome = ImportOutcome::default();
        assert_eq!(outcome.summary(), "Nothing new to import.");
        outcome.imported[ImportGroup::Tabs.index()] = 6;
        outcome.imported[ImportGroup::Highlights.index()] = 23;
        outcome.imported[ImportGroup::Nicknames.index()] = 1;
        outcome.already_present[ImportGroup::Highlights.index()] = 2;
        outcome.skipped = (0..4)
            .map(|index| SkippedItem {
                group: Some(ImportGroup::Tabs),
                item: format!("item {index}"),
                reason: "why".to_string(),
            })
            .collect();
        assert_eq!(
            outcome.summary(),
            "Imported 6 tabs, 23 highlights and 1 nickname. Skipped 4: item 0 (why); \
             item 1 (why); item 2 (why); 1 more. 2 were already in Echo."
        );
        assert_eq!(outcome.total_imported(), 30);
        outcome.repeated[ImportGroup::Tabs.index()] = 1;
        outcome.backup_files = vec![
            "settings-before-import-1.json".to_string(),
            "tabs-before-chatterino-import-1.json".to_string(),
        ];
        assert!(outcome.summary().ends_with(
            " 1 repeated entry in Chatterino was imported once. Backed up \
             settings-before-import-1.json and tabs-before-chatterino-import-1.json in the \
             settings-backups folder."
        ));
    }

    #[test]
    fn chatterino_import_translates_qt_replacements() {
        assert_eq!(qt_replacement_to_rust(r"\1-\12"), "${1}-${12}");
        assert_eq!(qt_replacement_to_rust("$1 costs $"), "$$1 costs $$");
        assert_eq!(qt_replacement_to_rust(r"a\b"), r"a\b");
        assert_eq!(qt_replacement_to_rust("emoji \u{1F600}"), "emoji \u{1F600}");
        assert_eq!(qt_backreference_at(r"x\2"), Some(1));
        assert_eq!(qt_backreference_at(r"x\y"), None);
    }

    #[test]
    fn chatterino_import_mod_button_key_normalizes_timeouts() {
        assert_eq!(
            mod_button_key("/timeout {user.name} 10m"),
            mod_button_key("/timeout  {user.name}   600")
        );
        assert_eq!(
            mod_button_key(".timeout {user.name} 1h spam"),
            "/timeout {user.name} 3600 spam"
        );
        assert_ne!(
            mod_button_key("/timeout {user.name} 1d"),
            mod_button_key("/timeout {user.name} 600")
        );
        assert_eq!(mod_button_key("/ban  {user.name}"), "/ban {user.name}");
    }

    #[test]
    fn chatterino_import_gives_zero_flex_a_small_share() {
        let layout = r#"{"windows": [{"type": "main", "tabs": [{"splits2": {
            "type": "horizontal",
            "items": [
                {"type": "split", "data": {"type": "twitch", "name": "a"}, "flexh": 0.0, "flexv": 1.0},
                {"type": "split", "data": {"type": "twitch", "name": "b"}, "flexh": 1.0, "flexv": 1.0}
            ]
        }}]}]}"#;
        let plan = plan(None, Some(layout), None);
        let (_, _, ratios) = split_parts(&plan.tabs[0].layout);
        assert!(ratios[0] > 0.0 && ratios[0] < 0.1, "{ratios:?}");
        assert!((ratios[0] + ratios[1] - 1.0).abs() < 1e-6);
    }

    #[test]
    fn chatterino_import_notices_source_changes_after_the_preview() {
        let dir = std::env::temp_dir().join(format!(
            "echo-chatterino-stale-test-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(SETTINGS_FILE), "{}").unwrap();
        let plan = load_plan(&dir);
        assert!(!plan.sources_changed());
        std::fs::write(dir.join(COMMANDS_FILE), r#"{"commands": []}"#).unwrap();
        assert!(plan.sources_changed(), "a new file counts as a change");
        let plan = load_plan(&dir);
        std::fs::write(dir.join(SETTINGS_FILE), r#"{"nicknames": []}"#).unwrap();
        assert!(plan.sources_changed(), "a resized file counts as a change");
        assert!(
            !self::plan(Some("{}"), None, None).sources_changed(),
            "plans built from text have nothing to compare"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn chatterino_import_layout_key_ignores_order_case_and_placeholders() {
        let layout = SplitNode::split(
            SplitOrientation::Vertical,
            vec![
                SplitNode::leaf("Forsen".to_string()),
                SplitNode::leaf(crate::workspace::new_placeholder()),
                SplitNode::leaf("#alice".to_string()),
            ],
            vec![0.3, 0.3, 0.4],
        );
        assert_eq!(layout_channel_key(&layout), ["alice", "forsen"]);
    }
}
