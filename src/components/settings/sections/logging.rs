//! Logging settings section
//!
//! Controls for diagnostics and log output.

use crate::backend;
use crate::backend::settings;
use crate::components::settings::settings_navigation::SettingDescriptor;
use crate::components::settings::widgets::{
    SelectOption, SettingsPageUi, labelled_toggle, quiet_action_button, setting_reset, setting_row,
    setting_target, settings_defaults, settings_disclosure_with_layout, settings_group,
    settings_group_contents, settings_path_row, settings_select, settings_subheading,
    settings_summary_disclosure,
};
use crate::components::ui::ButtonExt;
use crate::components::ui::PressToClick;
use crate::components::ui::input::{Escape, Input, InputEvent, InputState};
use crate::theme::colors;
use crate::theme::tokens::{Radii, RadiiPixels};
use gpui::prelude::FluentBuilder;
use gpui::*;
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{LazyLock, Mutex, MutexGuard};
use std::time::{Duration, Instant};

const LOG_OPTIONS_DISCLOSURE: &str = "advanced-log-options";
const LOG_VIEWER_DISCLOSURE: &str = "advanced-log-viewer";
const HISTORY_FILES_DISCLOSURE: &str = "advanced-history-files";

const WRITE_READABLE_LOGS: SettingDescriptor = SettingDescriptor {
    id: "developer-tools.write-readable-logs",
    label: "Write readable channel logs",
    aliases: &["chat log", "text file", "per channel"],
    control_id: "chat-readable-logs",
    disclosures: &[],
};

const PERSIST_HISTORY: SettingDescriptor = SettingDescriptor {
    id: "developer-tools.persist-history",
    label: "Persist recent message history",
    aliases: &["history cache", "instant load", "restore"],
    control_id: "persist-history",
    disclosures: &[],
};

const LOG_SERVER_TIMESTAMPS: SettingDescriptor = SettingDescriptor {
    id: "developer-tools.log-server-timestamps",
    label: "Include server timestamps",
    aliases: &["timestamp", "tmi-sent-ts", "raw"],
    control_id: "log-server-timestamps",
    disclosures: &[LOG_OPTIONS_DISCLOSURE],
};

const LOG_BY_STREAM_ID: SettingDescriptor = SettingDescriptor {
    id: "developer-tools.log-by-stream-id",
    label: "Organize by stream id",
    aliases: &["stream id", "split", "session"],
    control_id: "log-by-stream-id",
    disclosures: &[LOG_OPTIONS_DISCLOSURE],
};

const LOG_DELETED_MESSAGES: SettingDescriptor = SettingDescriptor {
    id: "developer-tools.log-deleted-messages",
    label: "Log deleted messages",
    aliases: &["clearmsg", "removed", "deleted"],
    control_id: "log-deleted-messages",
    disclosures: &[LOG_OPTIONS_DISCLOSURE],
};

const READABLE_LOG_FORMAT: SettingDescriptor = SettingDescriptor {
    id: "developer-tools.readable-log-format",
    label: "Readable log file format",
    aliases: &["txt", "log", "extension"],
    control_id: "readable-log-format",
    disclosures: &[LOG_OPTIONS_DISCLOSURE],
};

const DELETED_MESSAGE_MODE: SettingDescriptor = SettingDescriptor {
    id: "developer-tools.deleted-message-mode",
    label: "When Twitch deletes a message",
    aliases: &["clearmsg", "hide", "dim", "strikethrough", "placeholder"],
    control_id: "deleted-message-mode",
    disclosures: &[],
};

const DELETED_MESSAGE_LIMIT: SettingDescriptor = SettingDescriptor {
    id: "developer-tools.deleted-message-limit",
    label: "Deleted-message log character limit",
    aliases: &["Deleted message limit", "character limit", "truncate"],
    control_id: "deleted-limit",
    disclosures: &[LOG_OPTIONS_DISCLOSURE],
};

const READABLE_LOG_SEARCH: SettingDescriptor = SettingDescriptor {
    id: "developer-tools.readable-log-search",
    label: "Search readable channel logs",
    aliases: &["Search logs", "find", "grep", "log viewer"],
    control_id: "logging-latest-log-search-field",
    disclosures: &[LOG_VIEWER_DISCLOSURE],
};

const READABLE_LOG_SEARCH_LATEST_SCOPE: SettingDescriptor = SettingDescriptor {
    id: "developer-tools.readable-log-search-scope-latest",
    label: "Search newest readable log",
    aliases: &["latest", "newest", "search scope"],
    control_id: "log-scope-newest",
    disclosures: &[LOG_VIEWER_DISCLOSURE],
};

const READABLE_LOG_SEARCH_GLOBAL_SCOPE: SettingDescriptor = SettingDescriptor {
    id: "developer-tools.readable-log-search-scope-global",
    label: "Search all readable logs",
    aliases: &["all logs", "global", "search scope"],
    control_id: "log-scope-all-logs",
    disclosures: &[LOG_VIEWER_DISCLOSURE],
};

/// Developer Tools › Chat logging.
pub const DEVELOPER_CHAT_LOGGING_SETTINGS: &[SettingDescriptor] = &[
    WRITE_READABLE_LOGS,
    PERSIST_HISTORY,
    LOG_SERVER_TIMESTAMPS,
    LOG_BY_STREAM_ID,
    LOG_DELETED_MESSAGES,
    READABLE_LOG_FORMAT,
    DELETED_MESSAGE_LIMIT,
    READABLE_LOG_SEARCH,
    READABLE_LOG_SEARCH_LATEST_SCOPE,
    READABLE_LOG_SEARCH_GLOBAL_SCOPE,
];

/// Moderation & filters › Deleted messages.
pub const MODERATION_DELETED_MESSAGES_SETTINGS: &[SettingDescriptor] = &[DELETED_MESSAGE_MODE];

const HISTORY_FOLDER: SettingDescriptor = SettingDescriptor {
    id: "developer-tools.history-folder",
    label: "Message history folder",
    aliases: &["directory", "path", "open", "copy"],
    control_id: "history-folder",
    disclosures: &[HISTORY_FILES_DISCLOSURE],
};

const HISTORY_CLEANUP: SettingDescriptor = SettingDescriptor {
    id: "developer-tools.history-cleanup",
    label: "Cleanup",
    aliases: &["clear logs", "delete exports", "purge"],
    control_id: "history-cleanup",
    disclosures: &[HISTORY_FILES_DISCLOSURE],
};

const LATEST_HISTORY_FILE: SettingDescriptor = SettingDescriptor {
    id: "developer-tools.latest-history-file",
    label: "Latest history file",
    aliases: &["export", "text export", "open latest"],
    control_id: "latest-history-file",
    disclosures: &[HISTORY_FILES_DISCLOSURE],
};

/// Developer Tools › History & cleanup.
pub const DEVELOPER_HISTORY_CLEANUP_SETTINGS: &[SettingDescriptor] =
    &[HISTORY_FOLDER, HISTORY_CLEANUP, LATEST_HISTORY_FILE];

pub(super) const DIAGNOSTICS_JSON: SettingDescriptor = SettingDescriptor {
    id: "developer-tools.diagnostics-json",
    label: "Copy diagnostics JSON",
    aliases: &["metrics", "clipboard", "report"],
    control_id: "copy-diagnostics-json",
    disclosures: &["advanced-diagnostics"],
};

pub(super) const DIAGNOSTICS_RESET: SettingDescriptor = SettingDescriptor {
    id: "developer-tools.diagnostics-reset",
    label: "Reset metrics",
    aliases: &["counters", "clear", "sample"],
    control_id: "reset-metrics",
    disclosures: &["advanced-diagnostics"],
};

#[derive(Clone, Copy, PartialEq, Eq)]
enum LoggingViewerField {
    Search,
}

#[derive(Clone, Copy, PartialEq, Eq, Default)]
enum LoggingSearchScope {
    #[default]
    Latest,
    Global,
}

#[derive(Clone)]
enum LoggingSearchResultsView {
    Latest(Result<backend::history_cache::ReadableLogSearchResults, String>),
    Global(Result<backend::history_cache::ReadableLogGlobalSearchResults, String>),
}

#[derive(Clone, Default)]
struct LoggingViewerState {
    search_query: String,
    search_scope: LoggingSearchScope,
    focused: Option<LoggingViewerField>,
    selected_match: usize,
}

static LOGGING_VIEWER_STATE: LazyLock<Mutex<HashMap<u64, LoggingViewerState>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn lock_logging_viewer_state(
    mutex: &Mutex<HashMap<u64, LoggingViewerState>>,
) -> (MutexGuard<'_, HashMap<u64, LoggingViewerState>>, bool) {
    match mutex.lock() {
        Ok(state) => (state, false),
        Err(poisoned) => {
            let mut state = poisoned.into_inner();
            state.clear();
            mutex.clear_poison();
            crate::log_ui!(warn, "Reset poisoned logging viewer state");
            (state, true)
        }
    }
}

struct LoggingViewerInput {
    input: Entity<InputState>,
    _subscription: Subscription,
}

thread_local! {
    static LOGGING_VIEWER_INPUTS: RefCell<HashMap<EntityId, LoggingViewerInput>> = RefCell::new(HashMap::new());
}

/// How long a completed scan stays authoritative before the next render
/// re-checks the log folder.
const LOG_SCAN_TTL: Duration = Duration::from_millis(1500);
/// The search field is typed one character at a time; collapse bursts.
const LOG_SEARCH_DEBOUNCE: Duration = Duration::from_millis(200);
/// Safety valve so a scan that never completes cannot wedge the pending slot.
const LOG_SCAN_STALL: Duration = Duration::from_secs(10);

#[derive(Clone, PartialEq, Eq)]
struct LoggingScanKey {
    scope: LoggingSearchScope,
    query: String,
    content: bool,
}

fn scan_key_matches(completed: &LoggingScanKey, requested: &LoggingScanKey) -> bool {
    completed == requested
}

#[derive(Clone)]
struct LoggingScanSnapshot {
    key: LoggingScanKey,
    latest_history_file: Option<PathBuf>,
    latest_readable_log_file: Option<PathBuf>,
    preview: Result<backend::history_cache::ReadableLogPreview, String>,
    search: Option<LoggingSearchResultsView>,
}

struct PendingLoggingScan {
    key: LoggingScanKey,
    started: Instant,
}

#[derive(Default)]
struct LoggingScanState {
    snapshot: Option<LoggingScanSnapshot>,
    completed_at: Option<Instant>,
    pending: Option<PendingLoggingScan>,
    generation: u64,
}

static LOGGING_SCAN_STATE: LazyLock<Mutex<HashMap<u64, LoggingScanState>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
static LOGGING_SCAN_GENERATION: AtomicU64 = AtomicU64::new(1);

fn next_logging_scan_generation() -> u64 {
    LOGGING_SCAN_GENERATION.fetch_add(1, Ordering::Relaxed)
}

fn lock_logging_scan_state(
    mutex: &Mutex<HashMap<u64, LoggingScanState>>,
) -> (MutexGuard<'_, HashMap<u64, LoggingScanState>>, bool) {
    match mutex.lock() {
        Ok(state) => (state, false),
        Err(poisoned) => {
            let mut state = poisoned.into_inner();
            state.clear();
            mutex.clear_poison();
            crate::log_ui!(warn, "Reset poisoned logging scan state");
            (state, true)
        }
    }
}

/// Return the newest completed scan, scheduling a refresh when the request key
/// changed or the cached folder listing aged out.
fn logging_scan_snapshot<T: 'static>(
    owner: u64,
    key: LoggingScanKey,
    cx: &mut Context<T>,
) -> Option<LoggingScanSnapshot> {
    let mut request = None;
    let snapshot = {
        let (mut states, _) = lock_logging_scan_state(&LOGGING_SCAN_STATE);
        let state = states.entry(owner).or_default();
        let pending_active = state.pending.as_ref().is_some_and(|pending| {
            pending.key == key && pending.started.elapsed() < LOG_SCAN_STALL
        });
        let key_matches = state.snapshot.as_ref().is_some_and(|snap| snap.key == key);
        let expired = state
            .completed_at
            .is_none_or(|at| at.elapsed() >= LOG_SCAN_TTL);

        if !pending_active && (!key_matches || expired) {
            let generation = next_logging_scan_generation();
            state.generation = generation;
            let debounce = state.snapshot.is_some() && !key_matches;
            state.pending = Some(PendingLoggingScan {
                key: key.clone(),
                started: Instant::now(),
            });
            request = Some((generation, key, debounce));
        }
        state.snapshot.clone()
    };

    if let Some((generation, key, debounce)) = request {
        spawn_logging_scan(owner, generation, key, debounce, cx);
    }

    snapshot
}

fn spawn_logging_scan<T: 'static>(
    owner: u64,
    generation: u64,
    key: LoggingScanKey,
    debounce: bool,
    cx: &mut Context<T>,
) {
    cx.spawn(async move |this, cx| {
        if debounce {
            cx.background_executor().timer(LOG_SEARCH_DEBOUNCE).await;
        }
        if !logging_scan_is_current(owner, generation) {
            return;
        }

        let snapshot = cx
            .background_executor()
            .spawn(async move { compute_logging_scan(key) })
            .await;

        if !store_logging_scan(owner, generation, snapshot) {
            return;
        }

        let _ = this.update(cx, |_this, cx| {
            crate::backend::metrics::record_ui_notify_source("settings.logging.scan");
            cx.notify();
        });
    })
    .detach();
}

/// Drop the cached scan (and cancel any in-flight one) after an action that
/// changes which files the scan would find.
pub(crate) fn invalidate_logging_scan() {
    let (mut states, _) = lock_logging_scan_state(&LOGGING_SCAN_STATE);
    for state in states.values_mut() {
        state.generation = next_logging_scan_generation();
        state.pending = None;
        state.snapshot = None;
        state.completed_at = None;
    }
}

pub(crate) fn invalidate_logging_scan_for_host(owner: EntityId) {
    invalidate_logging_scan_owner(owner.as_u64());
}

fn invalidate_logging_scan_owner(owner: u64) {
    let (mut states, _) = lock_logging_scan_state(&LOGGING_SCAN_STATE);
    let state = states.entry(owner).or_default();
    state.generation = next_logging_scan_generation();
    state.pending = None;
    state.snapshot = None;
    state.completed_at = None;
}

fn forget_logging_scan_for_host(owner: EntityId) {
    lock_logging_scan_state(&LOGGING_SCAN_STATE)
        .0
        .remove(&owner.as_u64());
}

fn logging_scan_is_current(owner: u64, generation: u64) -> bool {
    scan_generation_matches(
        &lock_logging_scan_state(&LOGGING_SCAN_STATE).0,
        owner,
        generation,
    )
}

fn scan_generation_matches(
    states: &HashMap<u64, LoggingScanState>,
    owner: u64,
    generation: u64,
) -> bool {
    states
        .get(&owner)
        .is_some_and(|state| state.generation == generation)
}
/// Publish a completed scan unless a newer request superseded it.
fn store_logging_scan(owner: u64, generation: u64, snapshot: LoggingScanSnapshot) -> bool {
    let (mut states, recovered) = lock_logging_scan_state(&LOGGING_SCAN_STATE);
    if recovered {
        return false;
    }
    if !scan_generation_matches(&states, owner, generation) {
        return false;
    }
    let Some(state) = states.get_mut(&owner) else {
        return false;
    };

    state.pending = None;
    state.snapshot = Some(snapshot);
    state.completed_at = Some(Instant::now());
    true
}

/// Runs off the UI thread: directory listing, tail preview, and search.
fn compute_logging_scan(key: LoggingScanKey) -> LoggingScanSnapshot {
    let search = (key.content && !key.query.is_empty()).then(|| match key.scope {
        LoggingSearchScope::Latest => LoggingSearchResultsView::Latest(
            backend::history_cache::search_latest_readable_log(&key.query, 64, 2 * 1024 * 1024),
        ),
        LoggingSearchScope::Global => {
            LoggingSearchResultsView::Global(backend::history_cache::search_readable_logs(
                &key.query,
                96,
                4 * 1024 * 1024,
                24,
                256 * 1024,
            ))
        }
    });

    LoggingScanSnapshot {
        latest_history_file: backend::history_cache::latest_history_file_path(),
        latest_readable_log_file: backend::history_cache::latest_readable_log_file_path(),
        preview: if key.content {
            backend::history_cache::latest_readable_log_preview(32, 128 * 1024)
        } else {
            Err("Open Search logs to load a preview.".to_string())
        },
        search,
        key,
    }
}

/// The two Advanced anchors this module owns.
///
/// The scan snapshot behind Readable Channel Logs and Latest History File is a
/// debounced one-shot keyed on the current query, so the viewer has one scan
/// per request rather than one per rendered row.
pub struct LoggingBlocks {
    /// Developer Tools > Chat logging.
    pub chat_logging: AnyElement,
    /// Developer Tools > History & cleanup.
    pub history_cleanup: AnyElement,
}

/// Build the History & logs and History files anchors used by Advanced.
///
/// File scans are lazy: opening the log viewer is the only path that starts a
/// background preview or search. The path and maintenance rows remain useful
/// even when no files have been written yet.
pub fn render_logging_section<T: 'static>(
    ui: &mut SettingsPageUi,
    window: &mut Window,
    cx: &mut Context<T>,
) -> LoggingBlocks {
    let defaults = settings_defaults();
    let chat_logging_enabled = settings::get_chat_logging_enabled();
    let persist_history_enabled = settings::get_persist_history_enabled();
    let log_server_timestamps = settings::get_log_server_timestamps();
    let log_by_stream_id = settings::get_log_by_stream_id();
    let log_deleted_messages = settings::get_log_deleted_messages();
    let log_format = settings::get_log_format();
    let deleted_log_char_limit = settings::get_deleted_log_char_limit().clamp(100, 5000) as u32;
    let stacked = ui.stacked;
    let options_disclosure = ui.disclosure(LOG_OPTIONS_DISCLOSURE, cx);
    let viewer_disclosure = ui.disclosure(LOG_VIEWER_DISCLOSURE, cx);
    let files_disclosure = ui.disclosure(HISTORY_FILES_DISCLOSURE, cx);
    let log_format_select = ui.select(READABLE_LOG_FORMAT.control_id, cx);
    let owner = cx.entity_id();
    let custom_log_dir = settings::get_log_directory();
    let readable_log_dir = backend::history_cache::readable_log_dir_path();
    let readable_log_dir_label = readable_log_dir
        .as_ref()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| "Readable log folder unavailable".to_string());

    if viewer_disclosure.is_expanded() {
        ensure_logging_viewer_input(window, cx);
    } else {
        release_logging_viewer_input(owner);
        update_logging_viewer_state_for(owner.as_u64(), |viewer| {
            viewer.focused = None;
        });
    }

    let viewer = logging_viewer_state_for(owner.as_u64());
    let search_query = viewer.search_query.trim().to_string();
    let scan_query = if viewer_disclosure.is_expanded() {
        search_query.clone()
    } else {
        String::new()
    };
    let request_key = LoggingScanKey {
        scope: viewer.search_scope,
        query: scan_query.clone(),
        content: viewer_disclosure.is_expanded(),
    };
    let scan = logging_scan_snapshot(owner.as_u64(), request_key.clone(), cx);
    let scan_matches_request = scan
        .as_ref()
        .is_some_and(|snapshot| scan_key_matches(&snapshot.key, &request_key));
    let scan_pending = !scan_matches_request;
    let latest_readable_log_preview = scan
        .as_ref()
        .filter(|_| scan_matches_request)
        .map(|snapshot| snapshot.preview.clone())
        .unwrap_or_else(|| Err("Scanning readable logs…".to_string()));
    let latest_history_file = scan
        .as_ref()
        .and_then(|snapshot| snapshot.latest_history_file.clone());
    let latest_readable_log_file = scan
        .as_ref()
        .and_then(|snapshot| snapshot.latest_readable_log_file.clone());
    let latest_history_file_label = latest_history_file
        .as_ref()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| {
            if scan_pending {
                "Scanning…"
            } else {
                "No saved channel history files yet"
            }
            .to_string()
        });
    let latest_readable_log_file_label = latest_readable_log_file
        .as_ref()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| {
            if scan_pending {
                "Scanning…"
            } else {
                "No readable channel logs yet"
            }
            .to_string()
        });
    let readable_log_search = if search_query.is_empty() || !scan_matches_request {
        None
    } else {
        scan.and_then(|snapshot| snapshot.search)
    };
    let search_count = readable_log_search.as_ref().map(|results| match results {
        LoggingSearchResultsView::Latest(Ok(results)) => Ok(results.matches.len()),
        LoggingSearchResultsView::Global(Ok(results)) => Ok(results.matches.len()),
        _ => Err(()),
    });
    let search_summary = log_search_summary(
        scan_pending,
        search_count,
        &search_query,
        viewer_disclosure.is_expanded(),
    );
    let files_summary = history_files_summary(
        scan_pending,
        latest_history_file.is_some(),
        latest_readable_log_file.is_some(),
    );
    let options_modified = usize::from(log_server_timestamps != defaults.log_server_timestamps)
        + usize::from(log_by_stream_id != defaults.log_by_stream_id)
        + usize::from(log_deleted_messages != defaults.log_deleted_messages)
        + usize::from(log_format != defaults.log_format)
        + usize::from(deleted_log_char_limit as usize != defaults.deleted_log_char_limit);
    let render_options = move |cx: &mut Context<T>| {
        div()
            .w_full()
            .min_w_0()
            .flex()
            .flex_col()
            .child(setting_target(
                LOG_SERVER_TIMESTAMPS,
                crate::components::settings::widgets::setting_compact_row(
                    LOG_SERVER_TIMESTAMPS.id,
                    LOG_SERVER_TIMESTAMPS.label,
                    Some("Store raw server-side timestamps in readable logs."),
                    labelled_toggle(
                        LOG_SERVER_TIMESTAMPS.control_id,
                        LOG_SERVER_TIMESTAMPS.label,
                        log_server_timestamps,
                        cx,
                        |enabled, cx| {
                            settings::set_log_server_timestamps(enabled);
                            cx.notify();
                        },
                    ),
                    log_server_timestamps != defaults.log_server_timestamps,
                    (log_server_timestamps != defaults.log_server_timestamps).then(|| {
                        setting_reset(
                            "reset-log-server-timestamps",
                            LOG_SERVER_TIMESTAMPS.label,
                            cx,
                            |cx| {
                                settings::set_log_server_timestamps(
                                    settings_defaults().log_server_timestamps,
                                );
                                cx.notify();
                            },
                        )
                    }),
                    stacked,
                ),
            ))
            .child(setting_target(
                LOG_BY_STREAM_ID,
                crate::components::settings::widgets::setting_compact_row(
                    LOG_BY_STREAM_ID.id,
                    LOG_BY_STREAM_ID.label,
                    Some("Start a new readable log when the stream id changes."),
                    labelled_toggle(
                        LOG_BY_STREAM_ID.control_id,
                        LOG_BY_STREAM_ID.label,
                        log_by_stream_id,
                        cx,
                        |enabled, cx| {
                            settings::set_log_by_stream_id(enabled);
                            cx.notify();
                        },
                    ),
                    log_by_stream_id != defaults.log_by_stream_id,
                    (log_by_stream_id != defaults.log_by_stream_id).then(|| {
                        setting_reset("reset-log-by-stream-id", LOG_BY_STREAM_ID.label, cx, |cx| {
                            settings::set_log_by_stream_id(settings_defaults().log_by_stream_id);
                            cx.notify();
                        })
                    }),
                    stacked,
                ),
            ))
            .child(setting_target(
                LOG_DELETED_MESSAGES,
                crate::components::settings::widgets::setting_compact_row(
                    LOG_DELETED_MESSAGES.id,
                    LOG_DELETED_MESSAGES.label,
                    Some("Keep deleted or removed messages in readable log files."),
                    labelled_toggle(
                        LOG_DELETED_MESSAGES.control_id,
                        LOG_DELETED_MESSAGES.label,
                        log_deleted_messages,
                        cx,
                        |enabled, cx| {
                            settings::set_log_deleted_messages(enabled);
                            cx.notify();
                        },
                    ),
                    log_deleted_messages != defaults.log_deleted_messages,
                    (log_deleted_messages != defaults.log_deleted_messages).then(|| {
                        setting_reset(
                            "reset-log-deleted-messages",
                            LOG_DELETED_MESSAGES.label,
                            cx,
                            |cx| {
                                settings::set_log_deleted_messages(
                                    settings_defaults().log_deleted_messages,
                                );
                                cx.notify();
                            },
                        )
                    }),
                    stacked,
                ),
            ))
            .child(setting_target(
                READABLE_LOG_FORMAT,
                setting_row(
                    READABLE_LOG_FORMAT.id,
                    READABLE_LOG_FORMAT.label,
                    Some("Choose whether new channel log files end in .txt or .log."),
                    settings_select(
                        READABLE_LOG_FORMAT.control_id,
                        READABLE_LOG_FORMAT.label,
                        &[
                            SelectOption::new("txt", ".txt"),
                            SelectOption::new("log", ".log"),
                        ],
                        &log_format,
                        &log_format_select,
                        cx,
                        |value, cx| {
                            settings::set_log_format(value);
                            cx.notify();
                        },
                    ),
                    log_format != defaults.log_format,
                    (log_format != defaults.log_format).then(|| {
                        setting_reset(
                            "reset-readable-log-format",
                            READABLE_LOG_FORMAT.label,
                            cx,
                            |cx| {
                                settings::set_log_format(settings_defaults().log_format.clone());
                                cx.notify();
                            },
                        )
                    }),
                    stacked,
                ),
            ))
            .child(setting_target(
                DELETED_MESSAGE_LIMIT,
                setting_row(
                    DELETED_MESSAGE_LIMIT.id,
                    DELETED_MESSAGE_LIMIT.label,
                    Some("Maximum characters written for each deleted message in readable logs."),
                    render_deleted_limit_row(deleted_log_char_limit, cx),
                    deleted_log_char_limit as usize != defaults.deleted_log_char_limit,
                    (deleted_log_char_limit as usize != defaults.deleted_log_char_limit).then(
                        || {
                            setting_reset(
                                "reset-deleted-log-character-limit",
                                DELETED_MESSAGE_LIMIT.label,
                                cx,
                                |cx| {
                                    settings::set_deleted_log_char_limit(
                                        settings_defaults().deleted_log_char_limit,
                                    );
                                    cx.notify();
                                },
                            )
                        },
                    ),
                    stacked,
                ),
            ))
    };
    let chat_logging = settings_group(
        "advanced-history-logs",
        "History & logs",
        true,
        settings_group_contents(
            "advanced-history-logs-content",
            "History & logs",
            false,
            div()
                .w_full()
                .min_w_0()
                .flex()
                .flex_col()
                .child(setting_target(
                    WRITE_READABLE_LOGS,
                    crate::components::settings::widgets::setting_compact_row(
                        WRITE_READABLE_LOGS.id,
                        WRITE_READABLE_LOGS.label,
                        Some("Append channel chat to per-channel daily text files."),
                        labelled_toggle(
                            WRITE_READABLE_LOGS.control_id,
                            WRITE_READABLE_LOGS.label,
                            chat_logging_enabled,
                            cx,
                            |enabled, cx| {
                                settings::set_chat_logging_enabled(enabled);
                                cx.notify();
                            },
                        ),
                        chat_logging_enabled != defaults.chat_logging_enabled,
                        (chat_logging_enabled != defaults.chat_logging_enabled).then(|| {
                            setting_reset(
                                "reset-readable-channel-logs",
                                WRITE_READABLE_LOGS.label,
                                cx,
                                |cx| {
                                    settings::set_chat_logging_enabled(
                                        settings_defaults().chat_logging_enabled,
                                    );
                                    cx.notify();
                                },
                            )
                        }),
                        stacked,
                    ),
                ))
                .child(setting_target(
                    PERSIST_HISTORY,
                    crate::components::settings::widgets::setting_compact_row(
                        PERSIST_HISTORY.id,
                        PERSIST_HISTORY.label,
                        Some("Save recent messages per channel so chat fills quickly when opened."),
                        labelled_toggle(
                            PERSIST_HISTORY.control_id,
                            PERSIST_HISTORY.label,
                            persist_history_enabled,
                            cx,
                            |enabled, cx| {
                                settings::set_persist_history_enabled(enabled);
                                cx.notify();
                            },
                        ),
                        persist_history_enabled != defaults.persist_history_enabled,
                        (persist_history_enabled != defaults.persist_history_enabled).then(|| {
                            setting_reset(
                                "reset-persist-recent-history",
                                PERSIST_HISTORY.label,
                                cx,
                                |cx| {
                                    settings::set_persist_history_enabled(
                                        settings_defaults().persist_history_enabled,
                                    );
                                    cx.notify();
                                },
                            )
                        }),
                        stacked,
                    ),
                ))
                .child(settings_disclosure_with_layout(
                    LOG_OPTIONS_DISCLOSURE,
                    "Log options",
                    Some("Timestamps, organization, deleted messages, and file format"),
                    options_modified,
                    &options_disclosure,
                    stacked,
                    cx,
                    move |cx| render_options(cx),
                )),
        ),
    )
    .into_any_element();

    let chat_logging = div()
        .w_full()
        .min_w_0()
        .flex()
        .flex_col()
        .gap(px(
            crate::components::settings::widgets::SettingsMetrics::GROUP_GAP,
        ))
        .child(chat_logging)
        .child(settings_group(
            "advanced-log-search-group",
            "Search logs",
            false,
            setting_target(
                READABLE_LOG_SEARCH,
                settings_summary_disclosure(
                    LOG_VIEWER_DISCLOSURE,
                    "Search logs",
                    &search_summary,
                    &viewer_disclosure,
                    cx,
                    move |cx| {
                        render_latest_log_preview_for_host(
                            owner.as_u64(),
                            latest_readable_log_preview,
                            readable_log_search,
                            viewer,
                            scan_pending,
                            cx,
                        )
                    },
                ),
            ),
        ))
        .into_any_element();

    let history_dir = backend::history_cache::history_dir_path();
    let history_dir_label = history_dir
        .as_ref()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| "History folder unavailable".to_string());
    let history_exports_dir = backend::history_cache::history_exports_dir_path();
    let history_exports_dir_label = history_exports_dir
        .as_ref()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| "History exports folder unavailable".to_string());
    let file_scan_status = if scan_pending {
        "Refreshing file paths…"
    } else if latest_history_file.is_some() || latest_readable_log_file.is_some() {
        "File paths refreshed"
    } else {
        "No history or readable log files found"
    };

    let render_files = move |cx: &mut Context<T>| {
        settings_group_contents(
            "advanced-history-files-content",
            "History files",
            false,
            div()
                .w_full()
                .min_w_0()
                .flex()
                .flex_col()
                .child(crate::components::settings::widgets::setting_compact_row(
                    "advanced-history-files-refresh",
                    "File status",
                    Some(file_scan_status),
                    quiet_action_button("refresh-history-file-status", "Refresh", false, cx, {
                        move |cx| {
                            invalidate_logging_scan_for_host(owner);
                            cx.notify();
                        }
                    }),
                    false,
                    None,
                    stacked,
                ))
                .child(setting_target(
                    HISTORY_FOLDER,
                    render_path_row(
                        "history-folder-row",
                        "Message history folder",
                        &history_dir_label,
                        div()
                            .flex()
                            .flex_wrap()
                            .justify_end()
                            .gap_2()
                            .child(quiet_action_button(
                                "open-history-folder",
                                "Open",
                                false,
                                cx,
                                move |cx| {
                                    if let Some(path) = history_dir.as_ref() {
                                        open_logging_directory(
                                            path.clone(),
                                            "message history folder",
                                        );
                                    }
                                    cx.notify();
                                },
                            ))
                            .child(quiet_action_button(
                                "copy-history-folder",
                                "Copy",
                                false,
                                cx,
                                {
                                    let history_dir_label = history_dir_label.clone();
                                    move |cx| {
                                        cx.write_to_clipboard(ClipboardItem::new_string(
                                            history_dir_label.clone(),
                                        ));
                                        cx.notify();
                                    }
                                },
                            )),
                        stacked,
                    ),
                ))
                .child(render_path_row(
                    "readable-log-folder-row",
                    "Readable log folder",
                    &readable_log_dir_label,
                    div()
                        .flex()
                        .flex_wrap()
                        .justify_end()
                        .gap_2()
                        .child(quiet_action_button(
                            "open-readable-log-folder",
                            "Open",
                            false,
                            cx,
                            move |cx| {
                                if let Some(path) = readable_log_dir.as_ref() {
                                    open_logging_directory(path.clone(), "readable log folder");
                                }
                                cx.notify();
                            },
                        ))
                        .child(quiet_action_button(
                            "copy-readable-log-folder",
                            "Copy",
                            false,
                            cx,
                            {
                                let readable_log_dir_label = readable_log_dir_label.clone();
                                move |cx| {
                                    cx.write_to_clipboard(ClipboardItem::new_string(
                                        readable_log_dir_label.clone(),
                                    ));
                                    cx.notify();
                                }
                            },
                        ))
                        .when(custom_log_dir.is_some(), |actions| {
                            actions.child(quiet_action_button(
                                "reset-readable-log-folder",
                                "Reset",
                                false,
                                cx,
                                move |cx| {
                                    settings::set_log_directory(None);
                                    invalidate_logging_scan();
                                    backend::push_ui_action(backend::UiAction::ShowSuccessToast {
                                        message: "Readable log folder reset".to_string(),
                                        detail: Some(
                                            "Echo will write future logs to the default folder"
                                                .to_string(),
                                        ),
                                    });
                                    cx.notify();
                                },
                            ))
                        }),
                    stacked,
                ))
                .child(render_path_row(
                    "latest-readable-log-row",
                    "Latest readable log",
                    &latest_readable_log_file_label,
                    div()
                        .flex()
                        .flex_wrap()
                        .justify_end()
                        .gap_2()
                        .child(quiet_action_button(
                            "open-latest-readable-log",
                            "Open",
                            false,
                            cx,
                            {
                                let latest_readable_log_file = latest_readable_log_file.clone();
                                move |cx| {
                                    if let Some(path) = latest_readable_log_file.as_ref() {
                                        open_logging_path(path.clone(), "latest readable log");
                                    }
                                    cx.notify();
                                }
                            },
                        ))
                        .child(quiet_action_button(
                            "copy-latest-readable-log",
                            "Copy",
                            false,
                            cx,
                            {
                                let latest_readable_log_file = latest_readable_log_file.clone();
                                move |cx| {
                                    if let Some(path) = latest_readable_log_file.as_ref() {
                                        cx.write_to_clipboard(ClipboardItem::new_string(
                                            path.display().to_string(),
                                        ));
                                    }
                                    cx.notify();
                                }
                            },
                        )),
                    stacked,
                ))
                .child(setting_target(
                    LATEST_HISTORY_FILE,
                    render_path_row(
                        "latest-history-file-row",
                        "Latest history file",
                        &latest_history_file_label,
                        div()
                            .flex()
                            .flex_wrap()
                            .justify_end()
                            .gap_2()
                            .child(quiet_action_button(
                                "open-latest-history-file",
                                "Open",
                                false,
                                cx,
                                {
                                    let latest_history_file = latest_history_file.clone();
                                    move |cx| {
                                        if let Some(path) = latest_history_file.as_ref() {
                                            open_logging_path(path.clone(), "latest history file");
                                        }
                                        cx.notify();
                                    }
                                },
                            ))
                            .child(quiet_action_button(
                                "copy-latest-history-file",
                                "Copy",
                                false,
                                cx,
                                {
                                    let latest_history_file = latest_history_file.clone();
                                    move |cx| {
                                        if let Some(path) = latest_history_file.as_ref() {
                                            cx.write_to_clipboard(ClipboardItem::new_string(
                                                path.display().to_string(),
                                            ));
                                        }
                                        cx.notify();
                                    }
                                },
                            ))
                            .child(quiet_action_button(
                                "export-latest-history-text",
                                "Export",
                                false,
                                cx,
                                |cx| {
                                    export_latest_history_text(false, cx);
                                },
                            ))
                            .child(quiet_action_button(
                                "open-latest-history-text",
                                "Open text",
                                false,
                                cx,
                                |cx| {
                                    export_latest_history_text(true, cx);
                                },
                            )),
                        stacked,
                    ),
                ))
                .child(render_path_row(
                    "history-exports-folder-row",
                    "History text exports folder",
                    &history_exports_dir_label,
                    quiet_action_button("open-history-exports-folder", "Open", false, cx, {
                        let history_exports_dir = history_exports_dir.clone();
                        move |cx| {
                            if let Some(path) = history_exports_dir.as_ref() {
                                if let Err(error) = std::fs::create_dir_all(path) {
                                    backend::push_ui_action(backend::UiAction::ShowWarningToast {
                                        message: "Could not create history exports folder"
                                            .to_string(),
                                        detail: Some(format!("{}: {error}", path.display())),
                                    });
                                } else {
                                    let path = path.clone();
                                    crate::utils::open_path(path.clone(), move |result| {
                                        if let Err(error) = result {
                                            backend::push_ui_action(
                                                backend::UiAction::ShowWarningToast {
                                                    message:
                                                        "Could not open history exports folder"
                                                            .to_string(),
                                                    detail: Some(format!(
                                                        "{}: {error}",
                                                        path.display()
                                                    )),
                                                },
                                            );
                                        }
                                    });
                                }
                            }
                            cx.notify();
                        }
                    }),
                    stacked,
                ))
                .child(setting_target(
                    HISTORY_CLEANUP,
                    div()
                        .w_full()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .child(settings_subheading("Maintenance"))
                        .child(crate::components::settings::widgets::setting_compact_row(
                            HISTORY_CLEANUP.id,
                            "Clear readable channel logs",
                            Some("Remove readable chat-log files from the log folder."),
                            quiet_action_button("clear-readable-logs", "Clear", true, cx, |_cx| {
                                backend::push_ui_action(
                                    backend::UiAction::ConfirmClearReadableLogs,
                                );
                            }),
                            false,
                            None,
                            stacked,
                        ))
                        .child(crate::components::settings::widgets::setting_compact_row(
                            "clear-history-text-exports",
                            "Clear history text exports",
                            Some("Remove exported history text files."),
                            quiet_action_button(
                                "clear-history-text-exports-action",
                                "Clear",
                                true,
                                cx,
                                |_cx| {
                                    backend::push_ui_action(
                                        backend::UiAction::ConfirmClearHistoryExports,
                                    );
                                },
                            ),
                            false,
                            None,
                            stacked,
                        )),
                )),
        )
    };
    let history_cleanup = settings_group(
        "advanced-history-files-group",
        "History files",
        true,
        settings_summary_disclosure(
            HISTORY_FILES_DISCLOSURE,
            "Files and folders",
            files_summary,
            &files_disclosure,
            cx,
            render_files,
        ),
    )
    .into_any_element();

    LoggingBlocks {
        chat_logging,
        history_cleanup,
    }
}

fn log_search_summary(
    pending: bool,
    count: Option<Result<usize, ()>>,
    query: &str,
    expanded: bool,
) -> String {
    if !expanded && !query.is_empty() {
        return format!("Retained query: {query}");
    }
    let status = if pending {
        "Scanning…".to_string()
    } else {
        match count {
            Some(Ok(0)) => "No matches".to_string(),
            Some(Ok(count)) => format!("{count} matches"),
            Some(Err(())) => "Search failed; open for details".to_string(),
            None => "Latest or all readable logs".to_string(),
        }
    };
    if query.is_empty() {
        status
    } else {
        format!("{query} · {status}")
    }
}

fn history_files_summary(pending: bool, history: bool, readable_logs: bool) -> &'static str {
    if pending {
        "Scanning…"
    } else {
        match (history, readable_logs) {
            (true, true) => "History and readable logs available",
            (true, false) => "Saved message history available",
            (false, true) => "Readable logs available",
            (false, false) => "No saved history or readable logs yet",
        }
    }
}

pub fn render_deleted_messages_section<T: 'static>(
    ui: &mut SettingsPageUi,
    cx: &mut Context<T>,
) -> impl IntoElement + use<T> {
    let mode = settings::get_deleted_message_mode();
    let default = settings_defaults().deleted_message_mode;
    let modified = mode != default;
    let select = ui.select(DELETED_MESSAGE_MODE.control_id, cx);
    settings_group(
        "moderation-deleted-messages",
        "Deleted messages",
        false,
        setting_target(
            DELETED_MESSAGE_MODE,
            setting_row(
                DELETED_MESSAGE_MODE.id,
                DELETED_MESSAGE_MODE.label,
                Some(
                    "Choose what stays visible after a deletion, timeout, ban, or chat clear. A split's \u{22ef} menu can override this for that split.",
                ),
                settings_select(
                    DELETED_MESSAGE_MODE.control_id,
                    DELETED_MESSAGE_MODE.label,
                    &[
                        SelectOption::new("disable", "Keep original"),
                        SelectOption::new("hide", "Hide"),
                        SelectOption::new("replace", "Placeholder (click to reveal)"),
                        SelectOption::new("dim", "Dim"),
                        SelectOption::new("strikethrough", "Strike through"),
                    ],
                    deleted_message_mode_value(mode),
                    &select,
                    cx,
                    |value, cx| {
                        settings::set_deleted_message_mode(deleted_message_mode_from_value(&value));
                        cx.notify();
                    },
                ),
                modified,
                modified.then(|| {
                    setting_reset(
                        "reset-deleted-message-mode",
                        DELETED_MESSAGE_MODE.label,
                        cx,
                        move |cx| {
                            settings::set_deleted_message_mode(default);
                            cx.notify();
                        },
                    )
                }),
                ui.stacked,
            ),
        ),
    )
}

fn logging_viewer_state_for(owner: u64) -> LoggingViewerState {
    lock_logging_viewer_state(&LOGGING_VIEWER_STATE)
        .0
        .get(&owner)
        .cloned()
        .unwrap_or_default()
}

fn update_logging_viewer_state_for(
    owner: u64,
    update: impl FnOnce(&mut LoggingViewerState),
) -> bool {
    update_logging_viewer_state_in(&LOGGING_VIEWER_STATE, owner, update)
}

fn update_logging_viewer_state_in(
    mutex: &Mutex<HashMap<u64, LoggingViewerState>>,
    owner: u64,
    update: impl FnOnce(&mut LoggingViewerState),
) -> bool {
    let (mut state, recovered) = lock_logging_viewer_state(mutex);
    if recovered {
        return false;
    }
    update(state.entry(owner).or_default());
    true
}

/// Drop the retained log-search input for a closing settings host. The search
/// draft remains available to that host until this lifecycle boundary.
pub(crate) fn release_logging_inputs_for_host(owner: EntityId) {
    release_logging_viewer_input(owner);
    forget_logging_scan_for_host(owner);
}

/// Forget the recoverable search draft only when the settings host closes.
/// Page changes release controls and scans but retain the host's query/scope.
pub(crate) fn forget_logging_viewer_state_for_host(owner: EntityId) {
    release_logging_inputs_for_host(owner);
    let (mut viewers, recovered) = lock_logging_viewer_state(&LOGGING_VIEWER_STATE);
    if !recovered {
        viewers.remove(&owner.as_u64());
    }
}

fn release_logging_viewer_input(owner: EntityId) {
    let removed = LOGGING_VIEWER_INPUTS
        .try_with(|inputs| inputs.borrow_mut().remove(&owner))
        .ok()
        .flatten();
    drop(removed);
}

fn logging_viewer_input<T: 'static>(cx: &Context<T>) -> Option<Entity<InputState>> {
    LOGGING_VIEWER_INPUTS.with(|inputs| {
        inputs
            .borrow()
            .get(&cx.entity_id())
            .map(|cached| cached.input.clone())
    })
}

fn ensure_logging_viewer_input<T: 'static>(window: &mut Window, cx: &mut Context<T>) {
    let owner = cx.entity_id();
    if let Some(input) = logging_viewer_input(cx) {
        let value = logging_viewer_state_for(owner.as_u64()).search_query;
        if input.read(cx).value() != value {
            input.update(cx, |input, cx| input.set_value(value, window, cx));
        }
        return;
    }

    let value = logging_viewer_state_for(owner.as_u64()).search_query;
    let input = cx.new(|cx| {
        InputState::new(window, cx)
            .with_placeholder("Search readable logs")
            .with_value(value)
    });
    let subscription = cx.subscribe_in(
        &input,
        window,
        move |_this, input, event: &InputEvent, window, cx| {
            match event {
                InputEvent::Focus => {
                    update_logging_viewer_state_for(owner.as_u64(), |viewer| {
                        viewer.focused = Some(LoggingViewerField::Search);
                    });
                }
                InputEvent::Change => {
                    let value = crate::components::ui::input::normalize_single_line_text(
                        input.read(cx).value(),
                    );
                    let snapshot = logging_viewer_state_for(owner.as_u64());
                    if snapshot.search_query != value {
                        update_logging_viewer_state_for(owner.as_u64(), |viewer| {
                            viewer.search_query = value.clone();
                            viewer.selected_match = 0;
                        });
                    }
                    if input.read(cx).value() != value {
                        input.update(cx, |input, cx| input.set_value(value, window, cx));
                    }
                }
                InputEvent::Blur => {
                    update_logging_viewer_state_for(owner.as_u64(), |viewer| {
                        if viewer.focused == Some(LoggingViewerField::Search) {
                            viewer.focused = None;
                        }
                    });
                }
                InputEvent::PressEnter => {}
            }
            cx.notify();
        },
    );
    LOGGING_VIEWER_INPUTS.with(|inputs| {
        inputs.borrow_mut().insert(
            owner,
            LoggingViewerInput {
                input,
                _subscription: subscription,
            },
        );
    });
}

fn deleted_message_mode_value(mode: settings::DeletedMessageMode) -> &'static str {
    match mode {
        settings::DeletedMessageMode::Disable => "disable",
        settings::DeletedMessageMode::Hide => "hide",
        settings::DeletedMessageMode::Replace => "replace",
        settings::DeletedMessageMode::Strikethrough => "strikethrough",
        settings::DeletedMessageMode::Dim => "dim",
    }
}

fn deleted_message_mode_from_value(value: &str) -> settings::DeletedMessageMode {
    match value {
        "hide" => settings::DeletedMessageMode::Hide,
        "replace" => settings::DeletedMessageMode::Replace,
        "strikethrough" => settings::DeletedMessageMode::Strikethrough,
        "dim" => settings::DeletedMessageMode::Dim,
        _ => settings::DeletedMessageMode::Disable,
    }
}

fn render_deleted_limit_row<T: 'static>(
    value: u32,
    cx: &mut Context<T>,
) -> impl IntoElement + use<T> {
    const MIN: u32 = 100;
    const MAX: u32 = 5000;
    const STEP: u32 = 100;

    div().flex().items_center().justify_end().child(
        div()
            .flex()
            .items_center()
            .gap_2()
            .child(
                div()
                    .id("deleted-limit-dec")
                    .role(Role::Button)
                    .map(|control| {
                        crate::components::settings::widgets::settings_keyboard_control_if_enabled(
                            control,
                            value > MIN,
                            cx,
                        )
                    })
                    .a11y_synthetic_children(
                        crate::components::ui::accessibility::apply_description,
                    )
                    .aria_label("Decrease deleted-log character limit")
                    .w(px(24.0))
                    .h(px(24.0))
                    .rounded(Radii::sm())
                    .bg(colors::bg_elevated())
                    .border_1()
                    .border_color(colors::border())
                    .cursor_pointer()
                    .hover(|s| {
                        if value > MIN {
                            s.bg(colors::bg_hover())
                        } else {
                            s
                        }
                    })
                    .on_accessible_activate(cx.listener(move |_this, _event, _window, cx| {
                        if value > MIN {
                            let new_value = value.saturating_sub(STEP).max(MIN) as usize;
                            settings::set_deleted_log_char_limit(new_value);
                            cx.notify();
                        }
                    }))
                    .when(value <= MIN, |s| s.disabled_button())
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(
                        div()
                            .text_sm()
                            .text_color(colors::text_secondary())
                            .child("-"),
                    ),
            )
            .child(
                div()
                    .w(px(64.0))
                    .text_center()
                    .text_sm()
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(colors::text_primary())
                    .child(value.to_string()),
            )
            .child(
                div()
                    .id("deleted-limit-inc")
                    .role(Role::Button)
                    .map(|control| {
                        crate::components::settings::widgets::settings_keyboard_control_if_enabled(
                            control,
                            value < MAX,
                            cx,
                        )
                    })
                    .a11y_synthetic_children(
                        crate::components::ui::accessibility::apply_description,
                    )
                    .aria_label("Increase deleted-log character limit")
                    .w(px(24.0))
                    .h(px(24.0))
                    .rounded(Radii::sm())
                    .bg(colors::bg_elevated())
                    .border_1()
                    .border_color(colors::border())
                    .cursor_pointer()
                    .hover(|s| {
                        if value < MAX {
                            s.bg(colors::bg_hover())
                        } else {
                            s
                        }
                    })
                    .on_accessible_activate(cx.listener(move |_this, _event, _window, cx| {
                        if value < MAX {
                            let new_value = (value + STEP).min(MAX) as usize;
                            settings::set_deleted_log_char_limit(new_value);
                            cx.notify();
                        }
                    }))
                    .when(value >= MAX, |s| s.disabled_button())
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(
                        div()
                            .text_sm()
                            .text_color(colors::text_secondary())
                            .child("+"),
                    ),
            ),
    )
}

fn render_latest_log_preview_for_host<T: 'static>(
    owner: u64,
    preview: Result<backend::history_cache::ReadableLogPreview, String>,
    search_results: Option<LoggingSearchResultsView>,
    viewer: LoggingViewerState,
    scan_pending: bool,
    cx: &mut Context<T>,
) -> AnyElement {
    match preview {
        Ok(preview) => {
            let path_label = preview.path.display().to_string();
            let line_count = preview.lines.len();
            let status = if preview.truncated {
                format!(
                    "Showing {} recent line(s) from {} bytes",
                    line_count, preview.byte_len
                )
            } else {
                format!(
                    "Showing {} line(s) from {} bytes",
                    line_count, preview.byte_len
                )
            };
            let preview_text = preview.lines.join("\n");
            let copy_text = preview_text.clone();
            let is_empty = preview.lines.is_empty();
            let visible_lines = if is_empty {
                vec!["No lines in latest log yet.".to_string()]
            } else {
                preview.lines
            };
            div()
                .w_full()
                .p_2()
                .rounded(Radii::sm())
                .bg(colors::bg_elevated())
                .border_1()
                .border_color(colors::border())
                .flex()
                .flex_col()
                .gap_2()
                .child(
                    div()
                        .flex()
                        .flex_wrap()
                        .items_start()
                        .gap_3()
                        .child(
                            div()
                                .min_w_0()
                                .flex_1()
                                .flex()
                                .flex_col()
                                .gap_1()
                                .child(
                                    div()
                                        .text_sm()
                                        .font_weight(FontWeight::MEDIUM)
                                        .text_color(colors::text_primary())
                                        .child("Latest log preview"),
                                )
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(colors::text_muted())
                                        .child(status),
                                ),
                        )
                        .child(
                            div()
                                .flex()
                                .flex_wrap()
                                .items_center()
                                .gap_2()
                                .child(render_inline_button("Open Log", cx, {
                                    let preview_path = preview.path.clone();
                                    move |cx| {
                                        let preview_path = preview_path.clone();
                                        crate::utils::open_path(
                                            preview_path.clone(),
                                            move |result| match result {
                                                Ok(_) => backend::push_ui_action(
                                                    backend::UiAction::ShowSuccessToast {
                                                        message: "Opened newest readable log"
                                                            .to_string(),
                                                        detail: Some(
                                                            preview_path.display().to_string(),
                                                        ),
                                                    },
                                                ),
                                                Err(error) => backend::push_ui_action(
                                                    backend::UiAction::ShowWarningToast {
                                                        message:
                                                            "Could not open newest readable log"
                                                                .to_string(),
                                                        detail: Some(format!(
                                                            "{}: {error}",
                                                            preview_path.display()
                                                        )),
                                                    },
                                                ),
                                            },
                                        );
                                        cx.notify();
                                    }
                                }))
                                .child(render_inline_button("Copy Preview", cx, move |cx| {
                                    cx.write_to_clipboard(ClipboardItem::new_string(
                                        copy_text.clone(),
                                    ));
                                    backend::push_ui_action(backend::UiAction::ShowSuccessToast {
                                        message: "Copied latest log preview".to_string(),
                                        detail: None,
                                    });
                                    cx.notify();
                                })),
                        ),
                )
                .child(
                    div()
                        .text_xs()
                        .font_family("monospace")
                        .text_color(colors::text_muted())
                        .text_ellipsis()
                        .child(path_label),
                )
                .child(render_latest_log_search_controls(
                    owner,
                    viewer.search_scope,
                    search_results,
                    scan_pending,
                    cx,
                ))
                .child(
                    div()
                        .id("latest-log-preview-scroll")
                        .w_full()
                        .max_h(px(280.0))
                        .px_3()
                        .py_2()
                        .rounded(Radii::sm())
                        .bg(colors::bg_plate())
                        .border_1()
                        .border_color(colors::border())
                        .overflow_y_scroll()
                        .text_xs()
                        .font_family("monospace")
                        .line_height(relative(1.35))
                        .text_color(if is_empty {
                            colors::text_muted()
                        } else {
                            colors::text_secondary()
                        })
                        .children(visible_lines.into_iter().map(|line| {
                            div().w_full().min_h(px(17.0)).text_ellipsis().child(line)
                        })),
                )
                .into_any_element()
        }
        Err(error) => div()
            .w_full()
            .p_2()
            .rounded(Radii::sm())
            .bg(colors::bg_elevated())
            .border_1()
            .border_color(colors::border())
            .flex()
            .flex_col()
            .gap_1()
            .child(
                div()
                    .text_sm()
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(colors::text_primary())
                    .child("Latest log preview"),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(colors::text_muted())
                    .child(error),
            )
            .child(render_latest_log_search_controls(
                owner,
                viewer.search_scope,
                search_results,
                scan_pending,
                cx,
            ))
            .into_any_element(),
    }
}

fn render_latest_log_search_controls<T: 'static>(
    owner: u64,
    search_scope: LoggingSearchScope,
    search_results: Option<LoggingSearchResultsView>,
    scan_pending: bool,
    cx: &mut Context<T>,
) -> impl IntoElement + use<T> {
    let latest_selected = search_scope == LoggingSearchScope::Latest;
    let global_selected = search_scope == LoggingSearchScope::Global;

    div()
        .flex()
        .flex_col()
        .gap_2()
        .child(
            div()
                .flex()
                .flex_wrap()
                .items_start()
                .gap_3()
                .child(
                    div()
                        .min_w_0()
                        .flex_1()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(
                            div()
                                .text_sm()
                                .text_color(colors::text_primary())
                                .child("Search readable logs"),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(colors::text_muted())
                                .child(
                                    "Type locally in this field. Ctrl+V pastes, Backspace deletes, Esc unfocuses.",
                                ),
                        ),
                )
                .child(
                    div()
                        .id("advanced-log-search-scope")
                        .role(Role::RadioGroup)
                        .flex()
                        .flex_wrap()
                        .items_center()
                        .gap_2()
                        .child(setting_target(
                            READABLE_LOG_SEARCH_LATEST_SCOPE,
                            render_scope_button("Newest", latest_selected, cx, move |cx| {
                                update_logging_viewer_state_for(owner, |viewer| {
                                    viewer.search_scope = LoggingSearchScope::Latest;
                                    viewer.selected_match = 0;
                                });
                                cx.notify();
                            }),
                        ))
                        .child(setting_target(
                            READABLE_LOG_SEARCH_GLOBAL_SCOPE,
                            render_scope_button("All logs", global_selected, cx, move |cx| {
                                update_logging_viewer_state_for(owner, |viewer| {
                                    viewer.search_scope = LoggingSearchScope::Global;
                                    viewer.selected_match = 0;
                                });
                                cx.notify();
                            }),
                        )),
                )
                .child(
                    div()
                        .flex()
                        .flex_wrap()
                        .items_center()
                        .gap_2()
                        .child(render_inline_button("Paste", cx, move |cx| {
                            if let Some(text) = crate::utils::read_text(cx)
                            {
                                update_logging_viewer_state_for(owner, |viewer| {
                                    let single_line = crate::components::ui::input::normalize_single_line_text(&text);
                                    if !single_line.is_empty() {
                                        viewer.search_query.push_str(&single_line);
                                        viewer.selected_match = 0;
                                    }
                                });
                            }
                            cx.notify();
                        }))
                        .child(render_inline_button("Clear", cx, move |cx| {
                            update_logging_viewer_state_for(owner, |viewer| {
                                viewer.search_query.clear();
                                viewer.focused = None;
                                viewer.selected_match = 0;
                            });
                            cx.notify();
                        }))
                        .child(render_inline_button("Refresh", cx, move |cx| {
                            invalidate_logging_scan_owner(owner);
                            cx.notify();
                        })),
                ),
        )
        .child(render_log_search_field(owner, cx))
        .when(scan_pending, |el| {
            el.child(
                div()
                    .text_xs()
                    .text_color(colors::text_muted())
                    .child("Scanning readable logs…"),
            )
        })
        .child(match search_results {
            Some(LoggingSearchResultsView::Latest(Ok(results))) => {
                render_log_search_results(owner, results, cx).into_any_element()
            }
            Some(LoggingSearchResultsView::Global(Ok(results))) => {
                render_global_log_search_results(owner, results, cx).into_any_element()
            }
            Some(LoggingSearchResultsView::Latest(Err(error)))
            | Some(LoggingSearchResultsView::Global(Err(error))) => div()
                .text_xs()
                .text_color(colors::text_muted())
                .child(error)
                .into_any_element(),
            None => div()
                .text_xs()
                .text_color(colors::text_muted())
                .child(match search_scope {
                    LoggingSearchScope::Latest => {
                        "Enter search text to filter the newest readable log without leaving Settings."
                    }
                    LoggingSearchScope::Global => {
                        "Enter search text to scan recent tails across multiple readable logs."
                    }
                })
                .into_any_element(),
        })
}

fn render_log_search_field<T: 'static>(
    owner: u64,
    cx: &mut Context<T>,
) -> impl IntoElement + use<T> {
    let input = logging_viewer_input(cx);
    div()
        .id(SharedString::from(READABLE_LOG_SEARCH.control_id))
        .w_full()
        .min_w_0()
        .font_family("monospace")
        .when_some(input, |field, input| {
            field
                .capture_action(cx.listener(move |_this, _: &Escape, window, cx| {
                    update_logging_viewer_state_for(owner, |viewer| {
                        viewer.focused = None;
                    });
                    window.focus_prev(cx);
                    cx.stop_propagation();
                    cx.notify();
                }))
                .child(Input::new(&input).accessibility_label(READABLE_LOG_SEARCH.label))
        })
}

fn render_log_search_results<T: 'static>(
    owner: u64,
    results: backend::history_cache::ReadableLogSearchResults,
    cx: &mut Context<T>,
) -> impl IntoElement + use<T> {
    let channel_label = results.channel.clone();
    let selected_match = logging_viewer_state_for(owner)
        .selected_match
        .min(results.matches.len().saturating_sub(1));
    let selected_entry = results.matches.get(selected_match).cloned();
    let matches_len = results.matches.len();
    let can_navigate = matches_len > 0;
    let status = if results.match_limit_hit {
        format!(
            "Showing the first {} match(es) from {} scanned line(s)",
            matches_len, results.scanned_line_count
        )
    } else if results.truncated {
        format!(
            "{} match(es) in the most recent {} bytes of a {} byte log",
            matches_len, results.scanned_byte_len, results.byte_len
        )
    } else {
        format!(
            "{} match(es) across {} scanned line(s)",
            matches_len, results.scanned_line_count
        )
    };
    let selected_line = selected_entry.as_ref().map(|entry| entry.line.clone());
    let selected_target = selected_entry.and_then(|entry| entry.jump_target);

    div()
        .flex()
        .flex_col()
        .gap_2()
        .child(
            div()
                .flex()
                .flex_wrap()
                .items_start()
                .gap_3()
                .child(
                    div()
                        .min_w_0()
                        .flex_1()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(
                            div()
                                .text_xs()
                                .text_color(colors::text_muted())
                                .child(status),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(colors::text_muted())
                                .child(results.path.display().to_string()),
                        ),
                )
                .child(
                    div()
                        .flex()
                        .flex_wrap()
                        .items_center()
                        .gap_2()
                        .child(render_inline_button("Prev", cx, move |cx| {
                            if can_navigate {
                                update_logging_viewer_state_for(owner, |viewer| {
                                    viewer.selected_match = if viewer.selected_match == 0 {
                                        matches_len - 1
                                    } else {
                                        viewer.selected_match - 1
                                    };
                                });
                            }
                            cx.notify();
                        }))
                        .child(render_inline_button("Next", cx, move |cx| {
                            if can_navigate {
                                update_logging_viewer_state_for(owner, |viewer| {
                                    viewer.selected_match =
                                        (viewer.selected_match + 1) % matches_len;
                                });
                            }
                            cx.notify();
                        }))
                        .child(render_inline_button("Copy Match", cx, {
                            let selected_line = selected_line.clone();
                            move |cx| {
                                if let Some(line) = selected_line.clone() {
                                    cx.write_to_clipboard(ClipboardItem::new_string(line));
                                    backend::push_ui_action(
                                        backend::UiAction::ShowSuccessToast {
                                            message: "Copied log match".to_string(),
                                            detail: None,
                                        },
                                    );
                                } else {
                                    backend::push_ui_action(
                                        backend::UiAction::ShowWarningToast {
                                            message: "No matching log line to copy".to_string(),
                                            detail: None,
                                        },
                                    );
                                }
                                cx.notify();
                            }
                        }))
                        .child(render_inline_button("Jump", cx, {
                            let selected_target = selected_target.clone();
                            move |cx| {
                                if let Some(target) = selected_target.clone() {
                                    backend::push_ui_action(backend::UiAction::JumpToMessage {
                                        channel: target.channel,
                                        message_id: target.message_id,
                                    });
                                } else {
                                    backend::push_ui_action(
                                        backend::UiAction::ShowWarningToast {
                                            message: "Selected log row cannot jump into chat"
                                                .to_string(),
                                            detail: Some(
                                                "This readable log row does not include a usable message id."
                                                    .to_string(),
                                            ),
                                        },
                                    );
                                }
                                cx.notify();
                            }
                        })),
                ),
        )
        .child(
            div()
                .id("latest-log-search-results-scroll")
                .w_full()
                .max_h(px(220.0))
                .px_2()
                .py_2()
                .rounded(Radii::sm())
                .bg(colors::bg_plate())
                .border_1()
                .border_color(colors::border())
                .overflow_y_scroll()
                .flex()
                .flex_col()
                .gap_1()
                .when(results.matches.is_empty(), |el| {
                    el.child(
                        div()
                            .text_xs()
                            .text_color(colors::text_muted())
                            .child("No matches in the newest readable log scan."),
                    )
                })
                .children(results.matches.into_iter().enumerate().map(|(index, entry)| {
                    let is_selected = index == selected_match;
                    let line_label = format!("Recent line {}", entry.scanned_line_index + 1);
                    let channel_label = channel_label.clone();
                    div()
                        .id(("log-search-result", index))
                        .hover(|s| s.bg(colors::bg_hover()))
                        .w_full()
                        .px_2()
                        .py_1()
                        .rounded(Radii::sm())
                        .bg(if is_selected {
                            colors::bg_elevated()
                        } else {
                            colors::bg_plate()
                        })
                        .border_1()
                        .border_color(if is_selected {
                            colors::accent()
                        } else {
                            colors::border()
                        })
                        .cursor_pointer()
                        .on_completed_click(
                            cx.listener(move |_this, _event, _window, cx| {
                                update_logging_viewer_state_for(owner, |viewer| {
                                    viewer.selected_match = index;
                                });
                                cx.notify();
                            }),
                        )
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .gap_1()
                                .child(
                                    div()
                                        .text_xs()
                                        .font_weight(FontWeight::MEDIUM)
                                        .text_color(if is_selected {
                                            colors::accent()
                                        } else {
                                            colors::text_muted()
                                        })
                                        .child(format!(
                                            "{line_label} · #{}{}",
                                            channel_label,
                                            if entry.jump_target.is_some() {
                                                " · jump ready"
                                            } else {
                                                ""
                                            }
                                        )),
                                )
                                .child(
                                    div()
                                        .text_xs()
                                        .font_family("monospace")
                                        .text_color(colors::text_secondary())
                                        .child(entry.line),
                                ),
                        )
                })),
        )
}

fn render_global_log_search_results<T: 'static>(
    owner: u64,
    results: backend::history_cache::ReadableLogGlobalSearchResults,
    cx: &mut Context<T>,
) -> impl IntoElement + use<T> {
    let selected_match = logging_viewer_state_for(owner)
        .selected_match
        .min(results.matches.len().saturating_sub(1));
    let selected_entry = results.matches.get(selected_match).cloned();
    let matches_len = results.matches.len();
    let can_navigate = matches_len > 0;
    let mut status_parts = Vec::new();

    status_parts.push(format!(
        "{} match(es) across {} scanned file(s)",
        matches_len, results.scanned_file_count
    ));
    status_parts.push(format!(
        "{} line(s), {} bytes scanned",
        results.scanned_line_count, results.scanned_byte_len
    ));
    if results.match_limit_hit {
        status_parts.push("match cap hit".to_string());
    }
    if results.file_limit_hit {
        status_parts.push(format!(
            "file cap hit before all {} readable logs",
            results.total_file_count
        ));
    }
    if results.byte_limit_hit {
        status_parts.push("byte budget exhausted".to_string());
    }
    if results.truncated && !results.file_limit_hit && !results.byte_limit_hit {
        status_parts.push("recent tails only".to_string());
    }

    let selected_line = selected_entry.as_ref().map(|entry| entry.line.clone());
    let selected_path = selected_entry.as_ref().map(|entry| entry.path.clone());
    let selected_target = selected_entry.and_then(|entry| entry.jump_target);

    div()
        .flex()
        .flex_col()
        .gap_2()
        .child(
            div()
                .flex()
                .flex_wrap()
                .items_start()
                .gap_3()
                .child(
                    div()
                        .min_w_0()
                        .flex_1()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(
                            div()
                                .text_xs()
                                .text_color(colors::text_muted())
                                .child(status_parts.join(" · ")),
                        ),
                )
                .child(
                    div()
                        .flex()
                        .flex_wrap()
                        .items_center()
                        .gap_2()
                        .child(render_inline_button("Prev", cx, move |cx| {
                            if can_navigate {
                                update_logging_viewer_state_for(owner, |viewer| {
                                    viewer.selected_match = if viewer.selected_match == 0 {
                                        matches_len - 1
                                    } else {
                                        viewer.selected_match - 1
                                    };
                                });
                            }
                            cx.notify();
                        }))
                        .child(render_inline_button("Next", cx, move |cx| {
                            if can_navigate {
                                update_logging_viewer_state_for(owner, |viewer| {
                                    viewer.selected_match =
                                        (viewer.selected_match + 1) % matches_len;
                                });
                            }
                            cx.notify();
                        }))
                        .child(render_inline_button("Open File", cx, {
                            let selected_path = selected_path.clone();
                            move |cx| {
                                if let Some(path) = selected_path.clone() {
                                    crate::utils::open_path(path.clone(), move |result| {
                                        if let Err(error) = result {
                                            backend::push_ui_action(
                                                backend::UiAction::ShowWarningToast {
                                                    message: "Could not open readable log"
                                                        .to_string(),
                                                    detail: Some(format!(
                                                        "{}: {error}",
                                                        path.display()
                                                    )),
                                                },
                                            );
                                        }
                                    });
                                } else {
                                    backend::push_ui_action(
                                        backend::UiAction::ShowWarningToast {
                                            message: "No matching readable log selected"
                                                .to_string(),
                                            detail: None,
                                        },
                                    );
                                }
                                cx.notify();
                            }
                        }))
                        .child(render_inline_button("Copy Match", cx, {
                            let selected_line = selected_line.clone();
                            move |cx| {
                                if let Some(line) = selected_line.clone() {
                                    cx.write_to_clipboard(ClipboardItem::new_string(line));
                                    backend::push_ui_action(
                                        backend::UiAction::ShowSuccessToast {
                                            message: "Copied log match".to_string(),
                                            detail: None,
                                        },
                                    );
                                } else {
                                    backend::push_ui_action(
                                        backend::UiAction::ShowWarningToast {
                                            message: "No matching log line to copy".to_string(),
                                            detail: None,
                                        },
                                    );
                                }
                                cx.notify();
                            }
                        }))
                        .child(render_inline_button("Jump", cx, {
                            let selected_target = selected_target.clone();
                            move |cx| {
                                if let Some(target) = selected_target.clone() {
                                    backend::push_ui_action(backend::UiAction::JumpToMessage {
                                        channel: target.channel,
                                        message_id: target.message_id,
                                    });
                                } else {
                                    backend::push_ui_action(
                                        backend::UiAction::ShowWarningToast {
                                            message: "Selected log row cannot jump into chat"
                                                .to_string(),
                                            detail: Some(
                                                "This readable log row does not include a usable channel/message id pair."
                                                    .to_string(),
                                            ),
                                        },
                                    );
                                }
                                cx.notify();
                            }
                        })),
                ),
        )
        .child(
            div()
                .id("global-log-search-results-scroll")
                .w_full()
                .max_h(px(260.0))
                .px_2()
                .py_2()
                .rounded(Radii::sm())
                .bg(colors::bg_plate())
                .border_1()
                .border_color(colors::border())
                .overflow_y_scroll()
                .flex()
                .flex_col()
                .gap_1()
                .when(results.matches.is_empty(), |el| {
                    el.child(
                        div()
                            .text_xs()
                            .text_color(colors::text_muted())
                            .child("No matches in the global readable log scan."),
                    )
                })
                .children(results.matches.into_iter().enumerate().map(|(index, entry)| {
                    let is_selected = index == selected_match;
                    let line_label = format!("Recent line {}", entry.scanned_line_index + 1);
                    let path_label = entry.path.display().to_string();
                    let jump_ready = entry.jump_target.is_some();
                    div()
                        .id(("log-global-search-result", index))
                        .hover(|s| s.bg(colors::bg_hover()))
                        .w_full()
                        .px_2()
                        .py_1()
                        .rounded(Radii::sm())
                        .bg(if is_selected {
                            colors::bg_elevated()
                        } else {
                            colors::bg_plate()
                        })
                        .border_1()
                        .border_color(if is_selected {
                            colors::accent()
                        } else {
                            colors::border()
                        })
                        .cursor_pointer()
                        .on_completed_click(
                            cx.listener(move |_this, _event, _window, cx| {
                                update_logging_viewer_state_for(owner, |viewer| {
                                    viewer.selected_match = index;
                                });
                                cx.notify();
                            }),
                        )
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .gap_1()
                                .child(
                                    div()
                                        .text_xs()
                                        .font_weight(FontWeight::MEDIUM)
                                        .text_color(if is_selected {
                                            colors::accent()
                                        } else {
                                            colors::text_muted()
                                        })
                                        .child(format!(
                                            "#{} · {line_label}{}",
                                            entry.channel,
                                            if jump_ready { " · jump ready" } else { "" }
                                        )),
                                )
                                .child(
                                    div()
                                        .text_xs()
                                        .font_family("monospace")
                                        .text_color(colors::text_muted())
                                        .text_ellipsis()
                                        .child(path_label),
                                )
                                .child(
                                    div()
                                        .text_xs()
                                        .font_family("monospace")
                                        .text_color(colors::text_secondary())
                                        .child(entry.line),
                                ),
                        )
                })),
        )
}

fn render_scope_button<T: 'static, F: Fn(&mut Context<T>) + 'static>(
    label: &str,
    selected: bool,
    cx: &mut Context<T>,
    on_click: F,
) -> impl IntoElement + use<T, F> {
    let on_click = Rc::new(on_click);
    let keyboard_click = on_click.clone();
    let entity = cx.entity();
    let focus = crate::components::settings::widgets::control_focus(
        &format!(
            "log-scope-{}",
            crate::components::settings::widgets::control_slug(label)
        ),
        cx,
    );
    let bg = if selected {
        colors::accent()
    } else {
        colors::bg_elevated()
    };
    let border = if selected {
        colors::accent()
    } else {
        colors::border()
    };
    let text = if selected {
        colors::text_on_accent_contrast()
    } else {
        colors::text_primary()
    };

    div()
        .id(SharedString::from(format!(
            "log-scope-{}",
            crate::components::settings::widgets::control_slug(label)
        )))
        .role(Role::RadioButton)
        .a11y_synthetic_children(crate::components::ui::accessibility::apply_description)
        .map(|element| {
            crate::components::ui::accessibility::accessible_focus(element, focus.clone())
        })
        .tab_index(0)
        .aria_label(label.to_string())
        .aria_selected(selected)
        .px_3()
        .py_1()
        .rounded(Radii::sm())
        .bg(bg)
        .border_1()
        .border_color(border)
        .cursor_pointer()
        .hover(move |s| {
            if selected {
                s.bg(colors::accent_hover())
            } else {
                s.bg(colors::bg_hover())
            }
        })
        .on_activate(cx.listener(move |_this, _event, _window, cx| {
            keyboard_click(cx);
        }))
        .on_a11y_action(AccessibleAction::Click, move |_, _, cx| {
            entity.update(cx, |_, cx| on_click(cx))
        })
        .child(
            div()
                .text_xs()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(text)
                .child(label.to_string()),
        )
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test;
    use std::sync::Arc;

    #[test]
    fn history_summary_reports_history_without_readable_logs() {
        assert_eq!(
            history_files_summary(false, true, false),
            "Saved message history available"
        );
        assert_eq!(
            history_files_summary(false, false, true),
            "Readable logs available"
        );
        assert_eq!(
            history_files_summary(false, true, true),
            "History and readable logs available"
        );
        assert_eq!(history_files_summary(true, false, false), "Scanning…");
        assert_eq!(
            history_files_summary(false, false, false),
            "No saved history or readable logs yet"
        );
    }

    #[test]
    fn search_summary_preserves_query_and_reports_scan_matches_and_errors() {
        assert_eq!(
            log_search_summary(false, None, "needle", false),
            "Retained query: needle"
        );
        assert_eq!(
            log_search_summary(true, None, "needle", true),
            "needle · Scanning…"
        );
        assert_eq!(
            log_search_summary(false, Some(Ok(2)), "needle", true),
            "needle · 2 matches"
        );
        assert_eq!(
            log_search_summary(false, Some(Ok(0)), "needle", true),
            "needle · No matches"
        );
        assert_eq!(
            log_search_summary(false, Some(Err(())), "needle", true),
            "needle · Search failed; open for details"
        );
    }

    #[test]
    fn viewer_poison_resets_state_and_drops_the_discovering_edit() {
        let state = Arc::new(Mutex::new(HashMap::from([(
            7,
            LoggingViewerState {
                search_query: "partial".to_string(),
                search_scope: LoggingSearchScope::Global,
                focused: Some(LoggingViewerField::Search),
                selected_match: 4,
            },
        )])));
        let poisoned = state.clone();
        let result = std::thread::spawn(move || {
            let mut guard = poisoned.lock().unwrap();
            guard.get_mut(&7).unwrap().search_query.push_str(" broken");
            panic!("poison logging viewer");
        })
        .join();
        assert!(result.is_err());

        assert!(!update_logging_viewer_state_in(&state, 7, |viewer| {
            viewer.search_query.push_str(" event");
        }));
        let guard = state.lock().unwrap();
        let viewer = guard.get(&7).cloned().unwrap_or_default();
        assert!(viewer.search_query.is_empty());
        assert!(matches!(viewer.search_scope, LoggingSearchScope::Latest));
        assert!(viewer.focused.is_none());
        assert_eq!(viewer.selected_match, 0);
        drop(guard);

        assert!(update_logging_viewer_state_in(&state, 7, |viewer| {
            viewer.search_query.push_str("fresh");
        }));
        assert_eq!(state.lock().unwrap().get(&7).unwrap().search_query, "fresh");
    }

    #[test]
    fn scan_poison_discards_cache_and_advances_generation() {
        let state = Arc::new(Mutex::new(HashMap::from([(
            7,
            LoggingScanState {
                snapshot: None,
                completed_at: Some(Instant::now()),
                pending: Some(PendingLoggingScan {
                    key: LoggingScanKey {
                        scope: LoggingSearchScope::Global,
                        query: "partial".to_string(),
                        content: true,
                    },
                    started: Instant::now(),
                }),
                generation: 7,
            },
        )])));
        let poisoned = state.clone();
        let result = std::thread::spawn(move || {
            let mut guard = poisoned.lock().unwrap();
            guard.get_mut(&7).unwrap().generation = 9;
            panic!("poison logging scan");
        })
        .join();
        assert!(result.is_err());

        let (scan, recovered) = lock_logging_scan_state(&state);
        assert!(recovered);
        assert!(scan.is_empty());
        drop(scan);
        assert!(!state.is_poisoned());
    }

    #[test]
    fn invalidated_host_rejects_an_old_scan_completion_after_a_new_request() {
        let owner = 41;
        let old_generation = next_logging_scan_generation();
        let new_generation = next_logging_scan_generation();
        let states = HashMap::from([(
            owner,
            LoggingScanState {
                generation: new_generation,
                ..LoggingScanState::default()
            },
        )]);

        assert!(!scan_generation_matches(&states, owner, old_generation));
        assert!(scan_generation_matches(&states, owner, new_generation));
    }

    #[test]
    fn stale_search_results_require_an_exact_scope_query_and_content_key() {
        let completed = LoggingScanKey {
            scope: LoggingSearchScope::Latest,
            query: "old query".to_string(),
            content: true,
        };
        assert!(!scan_key_matches(
            &completed,
            &LoggingScanKey {
                scope: LoggingSearchScope::Latest,
                query: "new query".to_string(),
                content: true,
            },
        ));
        assert!(!scan_key_matches(
            &completed,
            &LoggingScanKey {
                scope: LoggingSearchScope::Global,
                query: "old query".to_string(),
                content: true,
            },
        ));
        assert!(scan_key_matches(&completed, &completed));
    }
}

fn render_path_row<C: IntoElement>(
    id: &str,
    title: &str,
    description: &str,
    actions: C,
    stacked: bool,
) -> impl IntoElement + use<C> {
    settings_path_row(id, title, description, actions, stacked)
}

fn open_logging_directory(path: PathBuf, label: &'static str) {
    if let Err(error) = std::fs::create_dir_all(&path) {
        backend::push_ui_action(backend::UiAction::ShowWarningToast {
            message: format!("Could not create {label}"),
            detail: Some(format!("{}: {error}", path.display())),
        });
        return;
    }
    open_logging_path(path, label);
}

fn open_logging_path(path: PathBuf, label: &'static str) {
    crate::utils::open_path(path.clone(), move |result| {
        if let Err(error) = result {
            backend::push_ui_action(backend::UiAction::ShowWarningToast {
                message: format!("Could not open {label}"),
                detail: Some(format!("{}: {error}", path.display())),
            });
        }
    });
}

fn export_latest_history_text<T: 'static>(open_after_export: bool, cx: &mut Context<T>) {
    cx.spawn(async move |this, cx| {
        let result = cx
            .background_executor()
            .spawn(async move { backend::history_cache::export_latest_history_text_file() })
            .await;
        let _ = this.update(cx, |_this, cx| {
            match result {
                Ok(path) if open_after_export => {
                    crate::utils::open_path(path.clone(), move |open_result| match open_result {
                        Ok(()) => backend::push_ui_action(backend::UiAction::ShowSuccessToast {
                            message: "History text export opened".to_string(),
                            detail: Some(path.display().to_string()),
                        }),
                        Err(error) => {
                            backend::push_ui_action(backend::UiAction::ShowWarningToast {
                                message: "Could not open history export".to_string(),
                                detail: Some(format!("{}: {error}", path.display())),
                            })
                        }
                    });
                }
                Ok(path) => {
                    cx.write_to_clipboard(ClipboardItem::new_string(path.display().to_string()));
                    backend::push_ui_action(backend::UiAction::ShowSuccessToast {
                        message: "History text export ready".to_string(),
                        detail: Some(path.display().to_string()),
                    });
                }
                Err(error) => backend::push_ui_action(backend::UiAction::ShowWarningToast {
                    message: "Could not export history".to_string(),
                    detail: Some(error),
                }),
            }
            cx.notify();
        });
    })
    .detach();
}

fn render_inline_button<T: 'static, F: Fn(&mut Context<T>) + 'static>(
    label: &str,
    cx: &mut Context<T>,
    on_click: F,
) -> impl IntoElement + use<T, F> {
    let label = match label {
        "Open Log" => "Open log",
        "Copy Preview" => "Copy preview",
        "Copy Match" => "Copy match",
        "Open File" => "Open file",
        other => other,
    };
    quiet_action_button(
        format!(
            "advanced-log-viewer-{}",
            crate::components::settings::widgets::control_slug(label)
        ),
        label,
        false,
        cx,
        on_click,
    )
}
