//! One-shot startup lifecycle markers shared by the binary and renderer.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

static CLOCK: OnceLock<Instant> = OnceLock::new();

/// A startup boundary owned by production code. Each is logged at most once
/// per process, under the name the benchmark scripts match on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StartupMarker {
    ProcessCreated,
    PrewarmJoined,
    PrewarmDetached,
    GpuiHttpClientReady,
    UserColorsLoaded,
    SettingsLoaded,
    BackendInitialized,
    CachesInitialized,
    ThemeInitialized,
    MainWindowOpening,
    MainWindowCreated,
    FirstRootRender,
    LayoutRestored,
    FirstChatViewUsable,
    FirstTextPaint,
    FirstVisibleEmotePaint,
    VisibleEmotesComplete,
    StartupNetworkFallbackComplete,
    InitialCatalogsReady,
    CachedHistoryCommitted,
}

impl StartupMarker {
    const COUNT: usize = Self::CachedHistoryCommitted as usize + 1;

    /// The marker's name in the `echo.startup` log line.
    pub fn name(self) -> &'static str {
        match self {
            Self::ProcessCreated => "process_created",
            Self::PrewarmJoined => "prewarm_joined",
            Self::PrewarmDetached => "prewarm_detached",
            Self::GpuiHttpClientReady => "gpui_http_client_ready",
            Self::UserColorsLoaded => "user_colors_loaded",
            Self::SettingsLoaded => "settings_loaded",
            Self::BackendInitialized => "backend_initialized",
            Self::CachesInitialized => "caches_initialized",
            Self::ThemeInitialized => "theme_initialized",
            Self::MainWindowOpening => "main_window_opening",
            Self::MainWindowCreated => "main_window_created",
            Self::FirstRootRender => "first_root_render",
            Self::LayoutRestored => "layout_restored",
            Self::FirstChatViewUsable => "first_chat_view_usable",
            Self::FirstTextPaint => "first_text_paint",
            Self::FirstVisibleEmotePaint => "first_visible_emote_paint",
            Self::VisibleEmotesComplete => "visible_emotes_complete",
            Self::StartupNetworkFallbackComplete => "startup_network_fallback_complete",
            Self::InitialCatalogsReady => "initial_catalogs_ready",
            Self::CachedHistoryCommitted => "cached_history_committed",
        }
    }
}

static RECORDED: [AtomicBool; StartupMarker::COUNT] =
    [const { AtomicBool::new(false) }; StartupMarker::COUNT];

/// Initialize the monotonic startup clock at process entry.
pub fn initialize_clock() {
    CLOCK.get_or_init(Instant::now);
}

/// Emit a one-shot startup boundary owned by production.
pub fn record(marker: StartupMarker) {
    if RECORDED[marker as usize].swap(true, Ordering::AcqRel) {
        return;
    }
    let started = CLOCK.get_or_init(Instant::now);
    tracing::info!(
        target: "echo.startup",
        marker = marker.name(),
        elapsed_ms = started.elapsed().as_millis() as u64,
        "startup_marker"
    );
}
