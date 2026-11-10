//! Application configuration constants
//!
//! Centralized location for magic numbers and configurable values.

use std::time::Duration;

/// Compact height of the top application bar (custom titlebar).
pub const TITLE_BAR_COMPACT_HEIGHT_PX: f32 = 28.0;

/// Standard height of the top application bar (custom titlebar).
pub const TITLE_BAR_STANDARD_HEIGHT_PX: f32 = 36.0;

/// Height of the tab bar
pub const TAB_BAR_HEIGHT_PX: f32 = 28.0;

/// Standard height of a two-row header in the main split tree.
pub const MAIN_SPLIT_HEADER_STANDARD_HEIGHT_PX: f32 = 40.0;

/// Height of a plain single-line chat input container: one 13px/18px draft
/// line plus 2px of vertical padding per side and the 1px top border.
///
/// GPUI/taffy size styles are border-box, so the border is *inside* this value.
/// Parents therefore reserve exactly the measured surface and must not add a
/// separate border buffer on top of it.
pub const CHAT_INPUT_HEIGHT_PX: f32 = 32.0;

/// Height of the chat input container while the composer also paints its
/// context row (send-block warning, unified target, character counter): the
/// 18px draft line plus the 14px context row, 2px of vertical padding per side,
/// the 1px top border, and 1px of slack.
pub const CHAT_INPUT_CONTEXT_HEIGHT_PX: f32 = 38.0;

/// Explicit line box of that context row. Without it the 11px secondary text
/// claims a font-relative line height and pushes the draft line out of the
/// 38px surface; 14px keeps its descenders inside the clipped row.
pub const CHAT_INPUT_CONTEXT_ROW_HEIGHT_PX: f32 = 14.0;

/// Minimum interactive height of the editable field row inside the composer.
pub const CHAT_INPUT_FIELD_HEIGHT_PX: f32 = 28.0;

/// Height of the reply context bar rendered as a sibling *above* the composer.
///
/// The bar reserves layout space and shrinks the message canvas; it never
/// overlays chat content.
pub const CHAT_REPLY_BAR_HEIGHT_PX: f32 = 36.0;

/// Twitch chat message character limit used by the input counter and send guard.
pub const TWITCH_MESSAGE_CHAR_LIMIT: usize = 500;

/// Preferred width of the settings modal.
///
/// The larger preferred size is not a minimum: the modal is still fitted to the
/// viewport through `fit_overlay_to_viewport`, so small displays keep an 8px
/// window margin instead of overflowing.
pub const SETTINGS_MODAL_WIDTH_PX: f32 = 960.0;

/// Preferred height of the settings modal (see [`SETTINGS_MODAL_WIDTH_PX`]).
pub const SETTINGS_MODAL_HEIGHT_PX: f32 = 720.0;

/// Preferred size of the standalone settings popout window.
///
/// Only used for popouts that still carry the pre-density-pass default; an
/// explicitly resized window keeps its persisted size.
pub const SETTINGS_POPOUT_DEFAULT_WIDTH_PX: f32 = 960.0;

/// Preferred height of the standalone settings popout window.
pub const SETTINGS_POPOUT_DEFAULT_HEIGHT_PX: f32 = 720.0;

/// Legacy persisted popout size that predates the density pass. A popout still
/// sitting on exactly this size has never been deliberately resized by the
/// user, so it adopts the new default once.
pub const SETTINGS_POPOUT_LEGACY_DEFAULT_SIZE_PX: (f32, f32) = (850.0, 650.0);

/// Minimum size of the standalone settings popout window.
pub const SETTINGS_POPOUT_MIN_WIDTH_PX: f32 = 600.0;

/// Minimum height of the standalone settings popout window.
pub const SETTINGS_POPOUT_MIN_HEIGHT_PX: f32 = 400.0;

/// Width of the settings navigation sidebar on roomy settings surfaces.
///
/// A flat icon-and-label list leaves the content column room to breathe.
pub const SETTINGS_SIDEBAR_WIDTH_PX: f32 = 200.0;

/// Width of the settings navigation sidebar below the compact-layout breakpoint.
///
/// The list releases space at the 600×400 safety size.
pub const SETTINGS_COMPACT_SIDEBAR_WIDTH_PX: f32 = 180.0;

/// Settings-surface width at which the tree switches to its compact width.
pub const SETTINGS_COMPACT_LAYOUT_MAX_WIDTH_PX: f32 = 760.0;

/// Height of the fixed settings content header that renders the live
/// `Page › Anchor` breadcrumb.
///
/// The header carries two type sizes (15px page name, 13px anchor) on one
/// baseline row.
pub const SETTINGS_CONTENT_HEADER_HEIGHT_PX: f32 = 40.0;

/// Width of the sidebar (mentions/whispers)
pub const SIDEBAR_WIDTH_PX: f32 = 320.0;

/// Interval for checking Twitch events (milliseconds)
pub const EVENT_POLL_INTERVAL_MS: u64 = 250;

/// Interval for checking connection status (milliseconds)
pub const STATUS_POLL_INTERVAL_MS: u64 = 1000;

/// Interval for refreshing stream info (seconds)
pub const STREAM_INFO_INTERVAL_SECS: u64 = 30;

/// Get event poll interval as Duration
pub fn event_poll_interval() -> Duration {
    Duration::from_millis(EVENT_POLL_INTERVAL_MS)
}

/// Get status poll interval as Duration
pub fn status_poll_interval() -> Duration {
    Duration::from_millis(STATUS_POLL_INTERVAL_MS)
}

/// Get stream info interval as Duration
pub fn stream_info_interval() -> Duration {
    Duration::from_secs(STREAM_INFO_INTERVAL_SECS)
}

/// Maximum number of messages to keep per active channel in memory.
///
/// Kept at 1000 for sub-300MB normal-use targets, because message structs,
/// layout rows, hitboxes, and UI snapshots all scale with retained message
/// count.
pub const MAX_CACHED_MESSAGES: usize = 1_000;

/// Message retention for channels that are connected but not in the selected tab.
///
/// Equal to [`MAX_CACHED_MESSAGES`]: every channel keeps its full scrollback
/// regardless of tab visibility, because trimming inactive tabs harder makes
/// switched-away messages unrecoverable on switch-back. The extra retention is
/// cheap — message structs are ~300B each and their layouts
/// are separately LRU-bounded — so the rebalance machinery stays in place only
/// as a no-op safety valve rather than an active trim.
pub const MAX_INACTIVE_CACHED_MESSAGES: usize = 1_000;

/// Maximum number of message history entries (up/down arrow)
/// Used in EchoApp input history buffer
pub const MAX_MESSAGE_HISTORY: usize = 100;

/// Minimum history messages to fetch on reconnect gap-fill
pub const HISTORY_MIN_GAP_FILL: u32 = 50;

/// Maximum history messages to fetch on reconnect gap-fill
pub const HISTORY_MAX_GAP_FILL: u32 = 2000;

/// Estimated messages per second for high-velocity channels
/// Used to calculate adaptive history limits
pub const HISTORY_MSG_PER_SEC_ESTIMATE: u32 = 10;

/// Calculate adaptive history limit based on disconnect duration
///
/// Uses a formula that scales with disconnect duration while respecting bounds:
/// - Short disconnects (<5s): Minimum fetch (50 messages)
/// - Medium disconnects (5-50s): Linear scale with estimated message rate
/// - Long disconnects (>50s): Maximum fetch (500 messages)
///
/// PERF: This adaptive approach prevents over-fetching for brief network hiccups
/// while ensuring we capture all missed messages for longer disconnects.
pub fn calculate_adaptive_history_limit(disconnect_secs: u64) -> u32 {
    let estimated_missed = (disconnect_secs as u32).saturating_mul(HISTORY_MSG_PER_SEC_ESTIMATE);

    estimated_missed.clamp(HISTORY_MIN_GAP_FILL, HISTORY_MAX_GAP_FILL)
}

use crate::state::{SplitNode, TabState, TabVisibility};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::{fs, io::Write};

/// Get the directory for storing workspace layouts
fn get_layouts_dir() -> Result<PathBuf, std::io::Error> {
    let config_root = crate::benchmark_profile::config_root()
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "config dir not found"))?;
    let config_dir = config_root.join("layouts");

    if !config_dir.exists() {
        fs::create_dir_all(&config_dir)?;
    }

    Ok(config_dir)
}

pub(crate) fn atomic_write_json(
    file_path: &std::path::Path,
    content: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let parent = file_path.parent().ok_or("missing parent directory")?;
    fs::create_dir_all(parent)?;

    let tmp_path = file_path.with_extension("json.tmp");
    {
        let mut file = fs::File::create(&tmp_path)?;
        file.write_all(content.as_bytes())?;
    }

    if let Err(error) = crate::utils::replace_file(&tmp_path, file_path) {
        let _ = fs::remove_file(&tmp_path);
        return Err(error.into());
    }

    Ok(())
}

/// Load a workspace layout from disk
pub fn load_workspace_layout(name: &str) -> Result<SplitNode, Box<dyn std::error::Error>> {
    let layouts_dir = get_layouts_dir()?;
    let file_path = layouts_dir.join(format!("{}.json", name));

    load_layout_json(&file_path, "Workspace layout", None)
}

/// Tab descriptor for serialization (decoupled from TabState runtime fields)
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TabDescriptor {
    /// Unique identifier
    pub id: String,
    /// Custom title set by user
    pub custom_title: Option<String>,
    /// The split layout tree for this tab
    pub layout: SplitNode,
    /// Whether this tab was selected when saved
    pub selected: bool,
    /// Active split path within the tab
    #[serde(default)]
    pub active_split_path: Vec<usize>,
    /// Whether highlights are enabled
    pub highlights_enabled: bool,
    /// Whether the layout is locked
    pub is_locked: bool,
    /// Visibility mode
    pub visibility: TabVisibility,
    /// Mark as unread on new messages
    pub mark_unread_on_new: bool,
}

impl From<&TabState> for TabDescriptor {
    fn from(tab: &TabState) -> Self {
        Self {
            id: tab.id.clone(),
            custom_title: tab.custom_title.clone(),
            layout: (*tab.layout).clone(),
            selected: tab.is_selected,
            active_split_path: tab.valid_active_split_path(),
            highlights_enabled: tab.highlights_enabled,
            is_locked: tab.is_locked,
            visibility: tab.visibility.clone(),
            mark_unread_on_new: tab.mark_unread_on_new,
        }
    }
}

impl From<TabDescriptor> for TabState {
    fn from(desc: TabDescriptor) -> Self {
        use std::sync::Arc;
        Self {
            id: desc.id,
            custom_title: desc.custom_title,
            layout: Arc::new(desc.layout),
            is_selected: desc.selected,
            active_split_path: desc.active_split_path,
            highlights_enabled: desc.highlights_enabled,
            is_locked: desc.is_locked,
            visibility: desc.visibility,
            is_live: false,
            has_unread: false,
            highlight_state: crate::state::TabHighlightState::None,
            mark_unread_on_new: desc.mark_unread_on_new,
        }
    }
}

/// Get the tabs file path
fn get_tabs_file() -> Result<PathBuf, std::io::Error> {
    let layouts_dir = get_layouts_dir()?;
    Ok(layouts_dir.join("tabs.json"))
}

/// Path of `tabs.json`, for callers that back it up before a bulk change.
pub fn tabs_file_path() -> Result<PathBuf, std::io::Error> {
    get_tabs_file()
}

/// tabs.json loaded (or was absent, or this process wrote it).
const TABS_PRIMARY_GOOD: u8 = 0;
/// tabs.json failed to load and a quarantine copy of it exists.
const TABS_PRIMARY_PRESERVED: u8 = 1;
/// tabs.json failed to load and could not be copied aside yet.
const TABS_PRIMARY_UNPRESERVED: u8 = 2;
static TABS_PRIMARY_STATE: std::sync::atomic::AtomicU8 =
    std::sync::atomic::AtomicU8::new(TABS_PRIMARY_GOOD);

/// Save all tabs to disk
pub fn save_tabs(tabs: &[TabState]) -> Result<(), Box<dyn std::error::Error>> {
    use std::sync::atomic::Ordering;

    let file_path = get_tabs_file()?;
    let descriptors: Vec<TabDescriptor> = tabs.iter().map(TabDescriptor::from).collect();
    let json = serde_json::to_string_pretty(&descriptors)?;

    let primary_state = TABS_PRIMARY_STATE.load(Ordering::Acquire);
    if primary_state == TABS_PRIMARY_UNPRESERVED && file_path.exists() {
        if !crate::utils::preserve_unloadable_file(&file_path, "tabs file") {
            return Err(format!(
                "refusing to overwrite '{}': it failed to load and could not be preserved",
                file_path.display()
            )
            .into());
        }
        TABS_PRIMARY_STATE.store(TABS_PRIMARY_PRESERVED, Ordering::Release);
    }

    // A tabs.json that failed to load must never replace the backup.
    if primary_state == TABS_PRIMARY_GOOD
        && file_path.exists()
        && let Err(error) = crate::utils::rotate_known_good_backups(&file_path, |bytes| {
            serde_json::from_slice::<Vec<TabDescriptor>>(bytes).is_ok()
        })
    {
        crate::log_persist!(warn, "Failed to refresh tabs backups: {}", error);
    }

    atomic_write_json(&file_path, &json)?;
    TABS_PRIMARY_STATE.store(TABS_PRIMARY_GOOD, Ordering::Release);
    crate::log_ui!(
        debug,
        "Saved {} tabs to {}",
        tabs.len(),
        file_path.display()
    );
    Ok(())
}

const TABS_SAVE_QUIET: std::time::Duration = std::time::Duration::from_millis(250);
const TABS_SAVE_MAX_DELAY: std::time::Duration = std::time::Duration::from_secs(1);

enum TabsSaveRequest {
    Snapshot(std::sync::Arc<Vec<TabState>>),
    /// Written right away, after any queued snapshot, with the result reported.
    Reported(
        std::sync::Arc<Vec<TabState>>,
        std::sync::mpsc::Sender<Result<(), String>>,
    ),
    Flush(std::sync::mpsc::Sender<()>),
}

static TABS_SAVE_TX: std::sync::OnceLock<Option<std::sync::mpsc::Sender<TabsSaveRequest>>> =
    std::sync::OnceLock::new();

fn tabs_save_tx() -> Option<&'static std::sync::mpsc::Sender<TabsSaveRequest>> {
    TABS_SAVE_TX
        .get_or_init(|| {
            let (tx, rx) = std::sync::mpsc::channel::<TabsSaveRequest>();
            let spawned = std::thread::Builder::new()
                .name("tabs-save-worker".to_string())
                .spawn(move || run_tabs_save_worker(rx));
            match spawned {
                Ok(_) => Some(tx),
                Err(error) => {
                    crate::log_ui!(warn, "Failed to start tab save worker: {}", error);
                    None
                }
            }
        })
        .as_ref()
}

fn run_tabs_save_worker(rx: std::sync::mpsc::Receiver<TabsSaveRequest>) {
    use std::sync::mpsc::RecvTimeoutError;

    let mut pending: Option<std::sync::Arc<Vec<TabState>>> = None;
    let mut first_queued_at = std::time::Instant::now();
    loop {
        let request = if pending.is_some() {
            let deadline = (first_queued_at + TABS_SAVE_MAX_DELAY)
                .saturating_duration_since(std::time::Instant::now())
                .min(TABS_SAVE_QUIET);
            match rx.recv_timeout(deadline) {
                Ok(request) => Some(request),
                Err(RecvTimeoutError::Timeout) => None,
                Err(RecvTimeoutError::Disconnected) => {
                    write_pending_tabs(&mut pending);
                    return;
                }
            }
        } else {
            match rx.recv() {
                Ok(request) => Some(request),
                Err(_) => return,
            }
        };

        match request {
            Some(TabsSaveRequest::Snapshot(tabs)) => {
                if pending.is_none() {
                    first_queued_at = std::time::Instant::now();
                }
                pending = Some(tabs);
                if first_queued_at.elapsed() >= TABS_SAVE_MAX_DELAY {
                    write_pending_tabs(&mut pending);
                }
            }
            Some(TabsSaveRequest::Reported(tabs, report)) => {
                write_pending_tabs(&mut pending);
                let _ = report.send(save_tabs(&tabs).map_err(|error| error.to_string()));
            }
            Some(TabsSaveRequest::Flush(ack)) => {
                write_pending_tabs(&mut pending);
                let _ = ack.send(());
            }
            None => write_pending_tabs(&mut pending),
        }
    }
}

fn write_pending_tabs(pending: &mut Option<std::sync::Arc<Vec<TabState>>>) {
    if let Some(tabs) = pending.take()
        && let Err(error) = save_tabs(&tabs)
    {
        crate::log_ui!(warn, "Failed to save tabs: {}", error);
    }
}

/// Queue a tab snapshot for the background writer. Rapid changes coalesce
/// into one write; `flush_tab_saves` forces it out.
pub fn queue_save_tabs(tabs: std::sync::Arc<Vec<TabState>>) {
    let queued =
        tabs_save_tx().is_some_and(|tx| tx.send(TabsSaveRequest::Snapshot(tabs.clone())).is_ok());
    if !queued && let Err(error) = save_tabs(&tabs) {
        crate::log_ui!(warn, "Failed to save tabs: {}", error);
    }
}

/// Write any queued tab snapshot now and wait for it, up to `timeout`.
pub fn flush_tab_saves(timeout: std::time::Duration) {
    let Some(tx) = TABS_SAVE_TX.get().and_then(Option::as_ref) else {
        return;
    };
    let (ack_tx, ack_rx) = std::sync::mpsc::channel::<()>();
    if tx.send(TabsSaveRequest::Flush(ack_tx)).is_err() {
        return;
    }
    if ack_rx.recv_timeout(timeout).is_err() {
        crate::log_ui!(warn, "Timed out waiting for tab save flush");
    }
}

/// Write `tabs` through the background writer, after anything already queued,
/// and wait up to `timeout` for the result.
pub fn save_tabs_and_wait(
    tabs: std::sync::Arc<Vec<TabState>>,
    timeout: std::time::Duration,
) -> Result<(), String> {
    let Some(tx) = tabs_save_tx() else {
        return save_tabs(&tabs).map_err(|error| error.to_string());
    };
    let (report_tx, report_rx) = std::sync::mpsc::channel();
    tx.send(TabsSaveRequest::Reported(tabs, report_tx))
        .map_err(|_| "the tab save worker is unavailable".to_string())?;
    report_rx
        .recv_timeout(timeout)
        .map_err(|_| "timed out waiting for tabs to save".to_string())?
}

/// Load tabs from disk
pub fn load_tabs() -> Result<Vec<TabState>, Box<dyn std::error::Error>> {
    let file_path = get_tabs_file()?;
    if matches!(file_path.try_exists(), Ok(false))
        && !crate::utils::backup_generation_paths(&file_path)
            .iter()
            .any(|path| path.exists())
    {
        return Ok(Vec::new());
    }
    let descriptors: Vec<TabDescriptor> =
        load_layout_json(&file_path, "Tab layout", Some(&TABS_PRIMARY_STATE))?;
    Ok(descriptors.into_iter().map(TabState::from).collect())
}

fn load_layout_json<T: serde::de::DeserializeOwned>(
    file_path: &Path,
    what: &'static str,
    primary_state: Option<&std::sync::atomic::AtomicU8>,
) -> Result<T, Box<dyn std::error::Error>> {
    let parse = |path: &Path| -> Result<T, Box<dyn std::error::Error>> {
        Ok(serde_json::from_str(&fs::read_to_string(path)?)?)
    };
    let primary_error = match parse(file_path) {
        Ok(value) => {
            if let Some(state) = primary_state {
                state.store(TABS_PRIMARY_GOOD, std::sync::atomic::Ordering::Release);
            }
            return Ok(value);
        }
        Err(error) => error,
    };
    let primary_missing = primary_error
        .downcast_ref::<std::io::Error>()
        .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound);
    let backups = crate::utils::backup_generation_paths(file_path);
    if primary_missing && !backups.iter().any(|path| path.exists()) {
        return Err(primary_error);
    }
    let mut preserved = Vec::new();
    let mut unpreserved = Vec::new();
    if !primary_missing {
        match crate::utils::quarantine_corrupt_file(file_path) {
            Ok(path) => preserved.push(path),
            Err(_) => unpreserved.push(file_path.to_path_buf()),
        }
    }
    if let Some(state) = primary_state {
        state.store(
            if unpreserved.is_empty() {
                TABS_PRIMARY_PRESERVED
            } else {
                TABS_PRIMARY_UNPRESERVED
            },
            std::sync::atomic::Ordering::Release,
        );
    }
    for backup in backups {
        let error = match parse(&backup) {
            Ok(value) => {
                crate::backend::reliability::note_storage_recovery(
                    what,
                    file_path,
                    Some(&backup),
                    &preserved,
                    &unpreserved,
                );
                return Ok(value);
            }
            Err(error) => error,
        };
        if error
            .downcast_ref::<std::io::Error>()
            .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound)
        {
            continue;
        }
        crate::log_persist!(
            warn,
            "Layout backup '{}' is unusable: {}",
            backup.display(),
            describe_tabs_error(error.as_ref())
        );
        match crate::utils::quarantine_corrupt_file(&backup) {
            Ok(path) => preserved.push(path),
            Err(_) => unpreserved.push(backup),
        }
    }
    crate::backend::reliability::note_storage_recovery(
        what,
        file_path,
        None,
        &preserved,
        &unpreserved,
    );
    Err(format!(
        "{what} could not be loaded: {}",
        describe_tabs_error(primary_error.as_ref())
    )
    .into())
}

/// A load error for the log: JSON errors are reduced to their position so tab
/// contents are not echoed.
fn describe_tabs_error(error: &(dyn std::error::Error + 'static)) -> String {
    match error.downcast_ref::<serde_json::Error>() {
        Some(json_error) => crate::utils::json_error_summary(json_error),
        None => error.to_string(),
    }
}

/// Migrate existing workspace layout to new tab system
///
/// Attempts to load the old "default.json" layout and convert it to a single tab.
/// Returns None if no old layout exists.
pub fn migrate_workspace_to_tabs() -> Option<Vec<TabState>> {
    match load_workspace_layout("default") {
        Ok(layout) => {
            crate::log_ui!(info, "Migrating existing workspace layout to tab system");
            let mut tab = TabState::with_layout(layout);
            tab.is_selected = true;
            Some(vec![tab])
        }
        Err(e) => {
            crate::log_ui!(debug, "No existing layout to migrate: {}", e);
            None
        }
    }
}
